use std::time::{Duration, Instant};

use bytes::Bytes;

use super::{terminal_targets::TerminalTargetError, App, PendingTabAutoStartAgent};
use crate::api::schema::AgentStartParams;

const DEFAULT_AGENT_START_TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) const MAX_AGENT_START_TIMEOUT: Duration = Duration::from_secs(300);
pub(crate) const AGENT_START_SETTLE_DELAY: Duration = Duration::from_secs(3);
const INVALID_AGENT_TIMEOUT_MESSAGE: &str =
    "agent start timeout must be greater than 3000ms and at most 300000ms";
const INVALID_AGENT_NAME_MESSAGE: &str = "agent name must start with a lowercase letter and contain only lowercase letters, digits, '-' or '_' (1-32 characters)";
fn valid_agent_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some('a'..='z'))
        && name.len() <= 32
        && chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '-' | '_'))
}

fn agent_launch_argv(kind: crate::detect::Agent, args: Vec<String>) -> Vec<String> {
    let mut argv = if kind == crate::detect::Agent::OpenCode && args.is_empty() {
        crate::agent_resume::opencode_local_server_argv()
    } else {
        vec![crate::detect::interactive_agent_executable(kind).to_string()]
    };
    argv.extend(args);
    argv
}

pub(crate) fn unused_agent_name<'a>(
    kind: crate::detect::Agent,
    mut number: u64,
    names: impl Iterator<Item = &'a str> + Clone,
) -> String {
    let label = crate::detect::agent_label(kind);
    loop {
        let name = format!("{label}-p{number}");
        if !names.clone().any(|existing| existing == name) {
            return name;
        }
        number += 1;
    }
}

impl App {
    pub(super) fn collect_agent_infos(&self) -> Vec<crate::api::schema::AgentInfo> {
        self.state
            .workspaces
            .iter()
            .enumerate()
            .flat_map(|(ws_idx, ws)| {
                ws.tabs.iter().flat_map(move |tab| {
                    tab.layout
                        .pane_ids()
                        .into_iter()
                        .filter_map(move |pane_id| self.agent_info(ws_idx, pane_id))
                })
            })
            .collect()
    }

    pub(super) fn reconcile_managed_agent_target(&mut self, target: &str) {
        let Ok(resolved) = self.resolve_agent_target(target) else {
            return;
        };
        let Some(terminal_id) = self
            .state
            .workspaces
            .get(resolved.ws_idx)
            .and_then(|workspace| workspace.terminal_id(resolved.pane_id))
            .cloned()
        else {
            return;
        };
        let changed = self
            .state
            .terminals
            .get_mut(&terminal_id)
            .is_some_and(|terminal| terminal.reconcile_managed_agent_at(Instant::now(), false));
        if changed {
            self.state.mark_session_dirty();
            self.schedule_session_save();
            self.emit_pane_updated(resolved.ws_idx, resolved.pane_id);
        }
    }

    pub(super) fn agent_info_for_target(
        &self,
        target: &str,
    ) -> Result<crate::api::schema::AgentInfo, TerminalTargetError> {
        let resolved = self.resolve_agent_target(target)?;
        self.agent_info(resolved.ws_idx, resolved.pane_id)
            .ok_or_else(|| TerminalTargetError::NotFound {
                target: target.to_string(),
            })
    }

    pub(super) fn focus_agent_target(
        &mut self,
        target: &str,
    ) -> Result<crate::api::schema::AgentInfo, TerminalTargetError> {
        let resolved = self.resolve_agent_target(target)?;
        self.state
            .focus_pane_in_workspace(resolved.ws_idx, resolved.pane_id);
        self.state.mark_active_tab_seen();
        self.state.mode = crate::app::Mode::Terminal;
        self.agent_info(resolved.ws_idx, resolved.pane_id)
            .ok_or_else(|| TerminalTargetError::NotFound {
                target: target.to_string(),
            })
    }

    pub(super) fn rename_agent_target(
        &mut self,
        target: &str,
        name: Option<String>,
    ) -> Result<crate::api::schema::AgentInfo, AgentRenameError> {
        let resolved = self
            .resolve_agent_target(target)
            .map_err(AgentRenameError::Target)?;
        let normalized_name = match name {
            Some(name) if valid_agent_name(&name) => Some(name),
            Some(_) => return Err(AgentRenameError::InvalidName),
            None => None,
        };

        if let Some(name) = normalized_name.as_deref() {
            let conflicts = self.agent_name_conflicts(name, &resolved.terminal_id);
            if !conflicts.is_empty() {
                return Err(AgentRenameError::DuplicateName {
                    name: name.to_string(),
                    candidates: conflicts,
                });
            }
        }

        let Some(terminal) = self
            .state
            .terminals
            .values_mut()
            .find(|terminal| terminal.id.to_string() == resolved.terminal_id)
        else {
            return Err(AgentRenameError::Target(TerminalTargetError::NotFound {
                target: target.to_string(),
            }));
        };
        if terminal.managed_agent_launch_pending() {
            return Err(AgentRenameError::PendingLaunch);
        }
        if terminal.effective_agent_label().is_none() {
            return Err(AgentRenameError::NotAgent);
        }
        match normalized_name {
            Some(name) => terminal.set_agent_name(name),
            None => terminal.clear_agent_name(),
        }
        self.state.mark_session_dirty();
        self.schedule_session_save();
        self.emit_pane_updated(resolved.ws_idx, resolved.pane_id);
        self.agent_info(resolved.ws_idx, resolved.pane_id)
            .ok_or_else(|| {
                AgentRenameError::Target(TerminalTargetError::NotFound {
                    target: target.to_string(),
                })
            })
    }

    pub(super) fn queue_tab_auto_start_agent(&mut self, ws_idx: usize, tab_idx: usize) {
        let Some(kind) = self.tab_auto_start_agent else {
            return;
        };
        let label = crate::detect::agent_label(kind);
        let Some(root_pane) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|workspace| workspace.tabs.get(tab_idx))
            .map(|tab| tab.root_pane)
        else {
            tracing::warn!(
                agent = label,
                "new tab has no root pane for agent auto-start"
            );
            return;
        };
        let Some(pane_id) = self.public_pane_id(ws_idx, root_pane) else {
            tracing::warn!(
                agent = label,
                "new tab root pane has no public id for agent auto-start"
            );
            return;
        };
        if self
            .pending_tab_auto_start_agents
            .iter()
            .any(|pending| pending.pane_id == pane_id)
        {
            return;
        }
        self.pending_tab_auto_start_agents
            .push(PendingTabAutoStartAgent {
                kind,
                pane_id,
                deadline: Instant::now() + DEFAULT_AGENT_START_TIMEOUT,
            });
        self.try_start_tab_auto_start_agents(Instant::now());
    }

    pub(super) fn queue_existing_tab_auto_start_agents(&mut self) {
        let mut eligible_tabs = Vec::new();
        for (ws_idx, workspace) in self.state.workspaces.iter().enumerate() {
            for (tab_idx, tab) in workspace.tabs.iter().enumerate() {
                let Some(terminal_id) = workspace.terminal_id(tab.root_pane) else {
                    continue;
                };
                let Some(terminal) = self.state.terminals.get(terminal_id) else {
                    continue;
                };
                if terminal.is_agent_terminal() || terminal.managed_agent_kind().is_some() {
                    continue;
                }
                let Some(runtime) = self.terminal_runtimes.get(terminal_id) else {
                    continue;
                };
                if available_shell_name(runtime).is_some() {
                    eligible_tabs.push((ws_idx, tab_idx));
                }
            }
        }

        for (ws_idx, tab_idx) in eligible_tabs {
            self.queue_tab_auto_start_agent(ws_idx, tab_idx);
        }
    }

    pub(crate) fn tab_auto_start_deadline(&self) -> Option<Instant> {
        self.pending_tab_auto_start_agents
            .iter()
            .map(|pending| pending.deadline)
            .min()
    }

    pub(crate) fn try_start_tab_auto_start_agents(&mut self, now: Instant) -> bool {
        let mut changed = false;
        let pending_agents = std::mem::take(&mut self.pending_tab_auto_start_agents);
        for PendingTabAutoStartAgent {
            kind,
            pane_id,
            deadline,
        } in pending_agents
        {
            let label = crate::detect::agent_label(kind);

            if now >= deadline {
                tracing::warn!(
                    agent = label,
                    pane_id,
                    "timed out waiting for the new tab shell before agent auto-start"
                );
                continue;
            }

            let Some((ws_idx, internal_pane_id)) = self.parse_current_public_pane_id(&pane_id)
            else {
                tracing::warn!(
                    agent = label,
                    pane_id,
                    "new tab agent auto-start pane is no longer available"
                );
                continue;
            };
            let Some(terminal_id) = self
                .state
                .workspaces
                .get(ws_idx)
                .and_then(|workspace| workspace.terminal_id(internal_pane_id))
                .cloned()
            else {
                tracing::warn!(
                    agent = label,
                    pane_id,
                    "new tab agent auto-start pane has no terminal"
                );
                continue;
            };
            let Some(runtime) = self.terminal_runtimes.get(&terminal_id) else {
                tracing::warn!(
                    agent = label,
                    pane_id,
                    "new tab agent auto-start pane has no live terminal"
                );
                continue;
            };
            let Some(shell_name) = available_shell_name(runtime) else {
                self.pending_tab_auto_start_agents
                    .push(PendingTabAutoStartAgent {
                        kind,
                        pane_id,
                        deadline,
                    });
                continue;
            };
            if !crate::platform::initial_pane_shell_ready_for_input(
                &shell_name,
                runtime.content_seq() > 0,
                runtime.has_reported_cwd(),
            ) {
                self.pending_tab_auto_start_agents
                    .push(PendingTabAutoStartAgent {
                        kind,
                        pane_id,
                        deadline,
                    });
                continue;
            }

            // Restored agents retain their names, but internal pane numbers can
            // be reused. Allocate against the current registry only when ready
            // to launch; earlier successful starts already reserve their names.
            let agents = self.collect_agent_infos();
            let name = unused_agent_name(
                kind,
                internal_pane_id.raw() as u64,
                agents.iter().filter_map(|agent| agent.name.as_deref()),
            );
            let params = AgentStartParams {
                name,
                kind: label.to_string(),
                pane_id,
                args: Vec::new(),
                timeout_ms: None,
            };

            match self.start_agent(params) {
                Ok((agent, _)) => {
                    tracing::info!(agent = label, pane_id = %agent.pane_id, "started configured agent in new tab");
                    changed = true;
                }
                Err(err) => {
                    let error = self.agent_start_error_body(err);
                    tracing::warn!(
                        agent = label,
                        code = error.code,
                        message = error.message,
                        "failed to start configured agent in new tab"
                    );
                }
            }
        }
        changed
    }

    pub(super) fn start_agent(
        &mut self,
        params: AgentStartParams,
    ) -> Result<(crate::api::schema::AgentInfo, Vec<String>), AgentStartError> {
        let name = params.name;
        if !valid_agent_name(&name) {
            return Err(AgentStartError::InvalidName);
        }
        let Some(kind) = crate::detect::parse_agent_label(&params.kind) else {
            return Err(AgentStartError::UnsupportedKind(params.kind));
        };
        if params
            .args
            .iter()
            .any(|arg| arg.chars().any(char::is_control))
        {
            return Err(AgentStartError::InvalidArgument);
        }
        let persisted_agent_session =
            crate::agent_resume::persisted_session_from_launch_args(kind, &params.args);
        let conflicts = self.agent_name_conflicts(&name, "");
        if !conflicts.is_empty() {
            return Err(AgentStartError::DuplicateName {
                name,
                candidates: conflicts,
            });
        }
        let Some((ws_idx, pane_id)) = self.parse_current_public_pane_id(&params.pane_id) else {
            return Err(AgentStartError::TargetNotFound(params.pane_id));
        };
        let terminal_id = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|workspace| workspace.terminal_id(pane_id))
            .cloned()
            .ok_or_else(|| AgentStartError::TargetNotFound(params.pane_id.clone()))?;
        let terminal = self
            .state
            .terminals
            .get(&terminal_id)
            .ok_or_else(|| AgentStartError::TargetNotFound(params.pane_id.clone()))?;
        if terminal.is_agent_terminal() || terminal.managed_agent_kind().is_some() {
            return Err(AgentStartError::TargetBusy(params.pane_id));
        }
        let runtime = self
            .terminal_runtimes
            .get(&terminal_id)
            .ok_or_else(|| AgentStartError::TargetUnavailable(params.pane_id.clone()))?;
        let shell_name = available_shell_name(runtime)
            .ok_or_else(|| AgentStartError::TargetBusy(params.pane_id.clone()))?;

        let argv = agent_launch_argv(kind, params.args);
        let command = crate::platform::interactive_shell_command(&argv, &shell_name)
            .ok_or(AgentStartError::InvalidArgument)?;
        let bytes = crate::app::api_helpers::encode_api_submission(runtime, &command);
        let timeout = Duration::from_millis(
            params
                .timeout_ms
                .unwrap_or(DEFAULT_AGENT_START_TIMEOUT.as_millis() as u64),
        );
        if timeout <= AGENT_START_SETTLE_DELAY || timeout > MAX_AGENT_START_TIMEOUT {
            return Err(AgentStartError::InvalidTimeout);
        }

        let now = Instant::now();
        let terminal = self
            .state
            .terminals
            .get_mut(&terminal_id)
            .ok_or_else(|| AgentStartError::TargetUnavailable(params.pane_id.clone()))?;
        terminal.begin_managed_agent(name.clone(), kind, now, AGENT_START_SETTLE_DELAY, timeout);
        if let Err(err) = runtime.try_send_bytes(Bytes::from(bytes)) {
            terminal.clear_agent_name();
            return Err(AgentStartError::InputFailed(err.to_string()));
        }
        if let Some(session) = persisted_agent_session {
            terminal.set_managed_agent_launch_session(session);
        }
        self.state.mark_session_dirty();
        self.schedule_session_save();

        let agent = self
            .agent_info(ws_idx, pane_id)
            .ok_or(AgentStartError::TargetUnavailable(params.pane_id))?;
        Ok((agent, argv))
    }

    pub(super) fn agent_start_error_body(
        &self,
        err: AgentStartError,
    ) -> crate::api::schema::ErrorBody {
        match err {
            AgentStartError::InvalidName => crate::api::schema::ErrorBody {
                code: "invalid_agent_name".into(),
                message: INVALID_AGENT_NAME_MESSAGE.into(),
            },
            AgentStartError::UnsupportedKind(kind) => crate::api::schema::ErrorBody {
                code: "unsupported_agent_kind".into(),
                message: format!("unsupported interactive agent kind {kind}"),
            },
            AgentStartError::InvalidArgument => crate::api::schema::ErrorBody {
                code: "invalid_agent_argument".into(),
                message: "agent arguments cannot be encoded safely for the target shell".into(),
            },
            AgentStartError::InvalidTimeout => crate::api::schema::ErrorBody {
                code: "invalid_agent_timeout".into(),
                message: INVALID_AGENT_TIMEOUT_MESSAGE.into(),
            },
            AgentStartError::TargetNotFound(target) => crate::api::schema::ErrorBody {
                code: "agent_pane_not_found".into(),
                message: format!("agent target pane {target} not found"),
            },
            AgentStartError::TargetBusy(target) => crate::api::schema::ErrorBody {
                code: "agent_pane_busy".into(),
                message: format!("agent target pane {target} is not an available shell"),
            },
            AgentStartError::TargetUnavailable(target) => crate::api::schema::ErrorBody {
                code: "agent_pane_unavailable".into(),
                message: format!("agent target pane {target} has no live terminal"),
            },
            AgentStartError::InputFailed(message) => crate::api::schema::ErrorBody {
                code: "agent_start_input_failed".into(),
                message,
            },
            AgentStartError::DuplicateName { name, candidates } => crate::api::schema::ErrorBody {
                code: "agent_name_taken".into(),
                message: format!(
                    "agent name {name} is already used; candidates: {}",
                    candidates
                        .into_iter()
                        .map(|candidate| format!(
                            "terminal_id={} pane_id={} workspace_id={} tab_id={} cwd={} status={:?}",
                            candidate.terminal_id,
                            candidate.pane_id,
                            candidate.workspace_id,
                            candidate.tab_id,
                            candidate.cwd.unwrap_or_else(|| "unknown".into()),
                            candidate.agent_status,
                        ))
                        .collect::<Vec<_>>()
                        .join("; ")
                ),
            },
        }
    }

    pub(super) fn agent_target_error_body(
        &self,
        err: TerminalTargetError,
    ) -> crate::api::schema::ErrorBody {
        match err {
            TerminalTargetError::NotFound { target } => crate::api::schema::ErrorBody {
                code: "agent_not_found".into(),
                message: format!("agent target {target} not found"),
            },
            TerminalTargetError::Ambiguous { target, candidates } => {
                crate::api::schema::ErrorBody {
                    code: "agent_target_ambiguous".into(),
                    message: format!(
                        "agent target {target} is ambiguous; candidates: {}",
                        candidates
                            .into_iter()
                            .map(|candidate| format!(
                                "terminal_id={} pane_id={} workspace_id={} tab_id={} cwd={} status={:?}",
                                candidate.terminal_id,
                                candidate.pane_id,
                                candidate.workspace_id,
                                candidate.tab_id,
                                candidate.cwd.unwrap_or_else(|| "unknown".into()),
                                candidate.agent_status,
                            ))
                            .collect::<Vec<_>>()
                            .join("; ")
                    ),
                }
            }
        }
    }

    pub(super) fn agent_rename_error_body(
        &self,
        err: AgentRenameError,
    ) -> crate::api::schema::ErrorBody {
        match err {
            AgentRenameError::Target(err) => self.agent_target_error_body(err),
            AgentRenameError::InvalidName => crate::api::schema::ErrorBody {
                code: "invalid_agent_name".into(),
                message: INVALID_AGENT_NAME_MESSAGE.into(),
            },
            AgentRenameError::NotAgent => crate::api::schema::ErrorBody {
                code: "agent_not_found".into(),
                message: "agent target does not currently host an agent".into(),
            },
            AgentRenameError::PendingLaunch => crate::api::schema::ErrorBody {
                code: "agent_launch_pending".into(),
                message: "agent name cannot change while startup is pending".into(),
            },
            AgentRenameError::DuplicateName { name, candidates } => crate::api::schema::ErrorBody {
                code: "agent_name_taken".into(),
                message: format!(
                    "agent name {name} is already used; candidates: {}",
                    candidates
                        .into_iter()
                        .map(|candidate| format!(
                            "terminal_id={} pane_id={} workspace_id={} tab_id={} cwd={} status={:?}",
                            candidate.terminal_id,
                            candidate.pane_id,
                            candidate.workspace_id,
                            candidate.tab_id,
                            candidate.cwd.unwrap_or_else(|| "unknown".into()),
                            candidate.agent_status,
                        ))
                        .collect::<Vec<_>>()
                        .join("; ")
                ),
            },
        }
    }

    pub(super) fn agent_info(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<crate::api::schema::AgentInfo> {
        let ws = self.state.workspaces.get(ws_idx)?;
        let pane_state = ws.pane_state(pane_id)?;
        let terminal = self.state.terminals.get(&pane_state.attached_terminal_id)?;
        if !terminal.is_agent_terminal() {
            return None;
        }
        let pane = self.pane_info(ws_idx, pane_id)?;
        Some(crate::api::schema::AgentInfo {
            terminal_id: pane.terminal_id,
            name: terminal.agent_name.clone(),
            agent: pane.agent,
            title: pane.title,
            terminal_title: pane.terminal_title,
            terminal_title_stripped: pane.terminal_title_stripped,
            display_agent: pane.display_agent,
            agent_status: pane.agent_status,
            screen_detection_skipped: terminal.full_lifecycle_hook_authority_active(),
            state_labels: pane.state_labels,
            tokens: pane.tokens,
            agent_session: pane.agent_session,
            workspace_id: pane.workspace_id,
            tab_id: pane.tab_id,
            pane_id: pane.pane_id,
            focused: pane.focused,
            launch_pending: terminal.managed_agent_launch_pending(),
            interactive_ready: terminal.managed_agent_interactive_ready(),
            state_change_seq: terminal.last_agent_state_change_seq.unwrap_or(0),
            cwd: pane.cwd,
            foreground_cwd: pane.foreground_cwd,
            revision: pane.revision,
        })
    }

    fn agent_name_conflicts(
        &self,
        name: &str,
        except_terminal_id: &str,
    ) -> Vec<crate::api::schema::AgentInfo> {
        self.collect_agent_infos()
            .into_iter()
            .filter(|agent| {
                agent.name.as_deref() == Some(name) && agent.terminal_id != except_terminal_id
            })
            .collect()
    }
}

fn available_shell_name(runtime: &crate::terminal::TerminalRuntime) -> Option<String> {
    #[cfg(test)]
    if runtime.child_pid().is_none() {
        return Some("sh".into());
    }
    crate::platform::available_pane_shell(runtime.child_pid()?)
}

pub(super) fn runtime_hosts_agent(
    runtime: &crate::terminal::TerminalRuntime,
    expected: crate::detect::Agent,
) -> bool {
    #[cfg(test)]
    if runtime.child_pid().is_none() {
        return true;
    }
    live_runtime_agent(runtime) == Some(expected)
}

fn live_runtime_agent(runtime: &crate::terminal::TerminalRuntime) -> Option<crate::detect::Agent> {
    let job = crate::detect::foreground_job(runtime.child_pid()?)?;
    crate::detect::identify_agent_in_job(&job)
        .map(|(agent, _)| agent)
        .or_else(|| {
            job.processes
                .iter()
                .find_map(|process| crate::platform::process_agent_hint(process.pid))
        })
}

pub(super) enum AgentStartError {
    InvalidName,
    UnsupportedKind(String),
    InvalidArgument,
    InvalidTimeout,
    TargetNotFound(String),
    TargetBusy(String),
    TargetUnavailable(String),
    InputFailed(String),
    DuplicateName {
        name: String,
        candidates: Vec<crate::api::schema::AgentInfo>,
    },
}

pub(super) enum AgentRenameError {
    Target(TerminalTargetError),
    InvalidName,
    NotAgent,
    PendingLaunch,
    DuplicateName {
        name: String,
        candidates: Vec<crate::api::schema::AgentInfo>,
    },
}

#[cfg(test)]
mod tests {
    use super::{agent_launch_argv, valid_agent_name};
    use std::time::Instant;

    #[test]
    fn agent_names_use_a_small_cli_safe_grammar() {
        for name in ["a", "reviewer-one", "reviewer_2", &"a".repeat(32)] {
            assert!(valid_agent_name(name), "expected {name:?} to be valid");
        }
        for name in [
            "",
            " reviewer",
            "reviewer ",
            "reviewer one",
            "Reviewer",
            "1reviewer",
            "reviewer.one",
            &"a".repeat(33),
        ] {
            assert!(!valid_agent_name(name), "expected {name:?} to be invalid");
        }
    }

    #[test]
    fn managed_opencode_root_exposes_only_an_ephemeral_local_server() {
        assert_eq!(
            agent_launch_argv(crate::detect::Agent::OpenCode, Vec::new()),
            ["opencode", "--hostname=127.0.0.1", "--port=0", "--no-mdns",]
        );
        assert_eq!(
            agent_launch_argv(
                crate::detect::Agent::OpenCode,
                vec!["attach".into(), "http://127.0.0.1:4096".into()],
            ),
            ["opencode", "attach", "http://127.0.0.1:4096"]
        );
    }

    #[tokio::test]
    async fn new_tab_agent_auto_start_reuses_managed_launch_for_each_root() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = super::super::App::new(
            &crate::config::Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let mut workspace = crate::workspace::Workspace::test_new("auto-start");
        let second_tab = workspace.test_add_tab(None);
        let first_root = workspace.tabs[0].root_pane;
        let second_root = workspace.tabs[second_tab].root_pane;
        app.state.workspaces = vec![workspace];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.tab_auto_start_agent = Some(crate::detect::Agent::OpenCode);

        let first_terminal = app.state.workspaces[0].tabs[0].panes[&first_root]
            .attached_terminal_id
            .clone();
        let second_terminal = app.state.workspaces[0].tabs[second_tab].panes[&second_root]
            .attached_terminal_id
            .clone();
        let (first_runtime, mut first_receiver) =
            crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        let (second_runtime, mut second_receiver) =
            crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        app.terminal_runtimes
            .insert(first_terminal.clone(), first_runtime);
        app.terminal_runtimes
            .insert(second_terminal.clone(), second_runtime);

        app.queue_tab_auto_start_agent(0, 0);
        app.queue_tab_auto_start_agent(0, second_tab);

        assert!(!app.try_start_tab_auto_start_agents(Instant::now()));
        assert_eq!(app.pending_tab_auto_start_agents.len(), 2);
        assert!(first_receiver.try_recv().is_err());
        assert!(second_receiver.try_recv().is_err());

        for terminal_id in [&first_terminal, &second_terminal] {
            app.terminal_runtimes
                .get(terminal_id)
                .unwrap()
                .test_process_pty_bytes(b"$ ");
        }
        assert!(app.try_start_tab_auto_start_agents(Instant::now()));
        assert!(app.pending_tab_auto_start_agents.is_empty());
        assert_eq!(
            app.state.terminals[&first_terminal].agent_name.as_deref(),
            Some(format!("opencode-p{}", first_root.raw()).as_str())
        );
        assert_eq!(
            app.state.terminals[&second_terminal].agent_name.as_deref(),
            Some(format!("opencode-p{}", second_root.raw()).as_str())
        );
        first_receiver
            .try_recv()
            .expect("managed launch should submit the initial tab agent command");
        second_receiver
            .try_recv()
            .expect("managed launch should submit the later tab agent command");

        let busy = app.start_agent(crate::api::schema::AgentStartParams {
            name: "shortcut-attempt".into(),
            kind: "opencode".into(),
            pane_id: app.public_pane_id(0, first_root).unwrap(),
            args: Vec::new(),
            timeout_ms: None,
        });
        assert!(matches!(busy, Err(super::AgentStartError::TargetBusy(_))));

        assert!(!app.try_start_tab_auto_start_agents(Instant::now()));
        assert!(first_receiver.try_recv().is_err());
        assert!(second_receiver.try_recv().is_err());

        let later_tab = app.state.workspaces[0].test_add_tab(None);
        let later_root = app.state.workspaces[0].tabs[later_tab].root_pane;
        app.state.ensure_test_terminals();
        let later_terminal = app.state.workspaces[0].tabs[later_tab].panes[&later_root]
            .attached_terminal_id
            .clone();
        let (later_runtime, mut later_receiver) =
            crate::terminal::TerminalRuntime::test_with_channel(80, 24);
        later_runtime.test_process_pty_bytes(b"$ ");
        app.terminal_runtimes
            .insert(later_terminal.clone(), later_runtime);

        // Model a restored agent retaining the name that a new internal pane
        // number would otherwise select. It must not prevent the new tab launch.
        let retained_name = format!("opencode-p{}", later_root.raw());
        app.state
            .terminals
            .get_mut(&first_terminal)
            .unwrap()
            .set_agent_name(retained_name.clone());
        app.queue_tab_auto_start_agent(0, later_tab);
        assert!(app.pending_tab_auto_start_agents.is_empty());
        later_receiver
            .try_recv()
            .expect("a retained name must not consume the new tab launch");
        assert_eq!(
            app.state.terminals[&first_terminal].agent_name.as_deref(),
            Some(retained_name.as_str())
        );
        let allocated_name = app.state.terminals[&later_terminal]
            .agent_name
            .as_deref()
            .unwrap();
        assert_ne!(allocated_name, retained_name);
        assert!(valid_agent_name(allocated_name));
        assert!(!app.try_start_tab_auto_start_agents(Instant::now()));
        assert!(later_receiver.try_recv().is_err());
    }
}
