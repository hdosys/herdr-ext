use std::{
    process::Stdio,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use tokio::io::AsyncReadExt;

use super::{
    state::{StatusCommandOutput, TabBarStatusSegment},
    App,
};
use crate::config::TabBarRightEntryConfig;
use crate::protocol::endpoint::StatusSpan;

const DATETIME_REFRESH_INTERVAL: Duration = Duration::from_secs(1);
const MAX_COMMAND_LINE_BYTES: usize = 4096;
const MAX_STATUS_TEXT_CHARS: usize = 80;

pub(super) struct TabBarDatetimeRuntime {
    segment_index: usize,
    format: time::format_description::OwnedFormatItem,
}

pub(super) struct TabBarCommandRuntime {
    segment_index: usize,
    command: String,
    interval: Duration,
    timeout: Duration,
    next_run_at: std::time::Instant,
    task: Option<StatusCommandTask>,
}

impl Drop for TabBarCommandRuntime {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort_handle.abort();
            // Kill the process group on the reconfiguring thread instead of waiting
            // for Tokio to schedule cancellation of the command task.
            task.control.terminate();
        }
    }
}

impl App {
    pub(super) fn configure_tab_bar_status(
        &mut self,
        entries: &[TabBarRightEntryConfig],
        separator: &str,
    ) {
        self.tab_bar_status_generation = self.tab_bar_status_generation.wrapping_add(1);
        self.tab_bar_datetimes.clear();
        self.tab_bar_commands.clear();
        self.state.tab_bar_right.clear();
        self.state.tab_bar_right_separator = sanitize_separator(separator);

        let now = std::time::Instant::now();
        for entry in entries
            .iter()
            .take(crate::config::MAX_TAB_BAR_RIGHT_ENTRIES)
        {
            match entry {
                TabBarRightEntryConfig::Zoom => {
                    self.state.tab_bar_right.push(TabBarStatusSegment::Zoom);
                }
                TabBarRightEntryConfig::Hostname => {
                    self.state
                        .tab_bar_right
                        .push(TabBarStatusSegment::Text(sanitize_status_text(
                            crate::platform::hostname().as_deref().unwrap_or_default(),
                        )));
                }
                TabBarRightEntryConfig::Datetime { format } => {
                    let Ok(format) = crate::config::parse_tab_bar_datetime_format(format) else {
                        continue;
                    };
                    let value = format_local_datetime(&format);
                    let segment_index = self.state.tab_bar_right.len();
                    self.state
                        .tab_bar_right
                        .push(TabBarStatusSegment::Text(value));
                    self.tab_bar_datetimes.push(TabBarDatetimeRuntime {
                        segment_index,
                        format,
                    });
                }
                TabBarRightEntryConfig::Text { text } => {
                    self.state
                        .tab_bar_right
                        .push(TabBarStatusSegment::Text(sanitize_literal_text(text)));
                }
                TabBarRightEntryConfig::Command {
                    command,
                    interval_seconds,
                    timeout_seconds,
                } => {
                    if !crate::platform::status_commands_supported()
                        || command.trim().is_empty()
                        || *interval_seconds == 0
                        || *interval_seconds > crate::config::MAX_TAB_BAR_COMMAND_INTERVAL_SECONDS
                        || *timeout_seconds == 0
                        || *timeout_seconds > crate::config::MAX_TAB_BAR_COMMAND_TIMEOUT_SECONDS
                    {
                        continue;
                    }
                    let segment_index = self.state.tab_bar_right.len();
                    self.state
                        .tab_bar_right
                        .push(TabBarStatusSegment::Text(None));
                    self.tab_bar_commands.push(TabBarCommandRuntime {
                        segment_index,
                        command: command.clone(),
                        interval: Duration::from_secs(*interval_seconds),
                        timeout: Duration::from_secs(*timeout_seconds),
                        next_run_at: now,
                        task: None,
                    });
                }
            }
        }

        self.next_tab_bar_datetime_refresh =
            (!self.tab_bar_datetimes.is_empty()).then_some(now + DATETIME_REFRESH_INTERVAL);
    }

    pub(crate) fn handle_tab_bar_status_tasks(&mut self, now: std::time::Instant) -> bool {
        let mut changed = false;

        if self
            .next_tab_bar_datetime_refresh
            .is_some_and(|deadline| now >= deadline)
        {
            for runtime in &self.tab_bar_datetimes {
                let value = format_local_datetime(&runtime.format);
                if let Some(TabBarStatusSegment::Text(current)) =
                    self.state.tab_bar_right.get_mut(runtime.segment_index)
                {
                    changed |= *current != value;
                    *current = value;
                }
            }
            self.next_tab_bar_datetime_refresh = Some(now + DATETIME_REFRESH_INTERVAL);
        }

        let command_due = self
            .tab_bar_commands
            .iter()
            .any(|runtime| runtime.task.is_none() && now >= runtime.next_run_at);
        if !command_due {
            return changed;
        }

        let generation = self.tab_bar_status_generation;
        let (environment, cwd) = self.custom_command_env();
        for runtime in &mut self.tab_bar_commands {
            if runtime.task.is_some() || now < runtime.next_run_at {
                continue;
            }
            runtime.next_run_at = now.checked_add(runtime.interval).unwrap_or(now);
            runtime.task = Some(spawn_status_command(
                self.event_tx.clone(),
                generation,
                runtime.segment_index,
                runtime.command.clone(),
                runtime.timeout,
                environment.clone(),
                cwd.clone(),
            ));
        }

        changed
    }

    pub(crate) fn next_tab_bar_status_deadline(&self) -> Option<std::time::Instant> {
        self.tab_bar_commands
            .iter()
            .filter(|runtime| runtime.task.is_none())
            .map(|runtime| runtime.next_run_at)
            .chain(self.next_tab_bar_datetime_refresh)
            .min()
    }

    pub(super) fn handle_tab_bar_command_finished(
        &mut self,
        generation: u64,
        segment_index: usize,
        result: Result<Option<StatusCommandOutput>, String>,
    ) -> bool {
        if generation != self.tab_bar_status_generation {
            return false;
        }
        let Some(runtime) = self
            .tab_bar_commands
            .iter_mut()
            .find(|runtime| runtime.segment_index == segment_index)
        else {
            return false;
        };
        runtime.task = None;

        let output = match result {
            Ok(output) => output,
            Err(error) => {
                tracing::warn!(command = %runtime.command, error, "tab bar status command failed");
                None
            }
        };
        let Some(current) = self.state.tab_bar_right.get_mut(segment_index) else {
            return false;
        };
        let segment = match output {
            Some(StatusCommandOutput { text, spans })
                if spans.iter().any(|span| span.url.is_some()) =>
            {
                TabBarStatusSegment::Link { text, spans }
            }
            output => TabBarStatusSegment::Text(output.map(|output| output.text)),
        };
        let changed = *current != segment;
        *current = segment;
        changed
    }
}

fn format_local_datetime(format: &time::format_description::OwnedFormatItem) -> Option<String> {
    let datetime = crate::platform::local_datetime()?;
    datetime
        .format(format)
        .ok()
        .and_then(|value| sanitize_status_text(&value))
}

fn sanitize_separator(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_control())
        .collect()
}

fn sanitize_literal_text(value: &str) -> Option<String> {
    let value: String = value
        .chars()
        .filter(|character| !character.is_control())
        .collect();
    (!value.is_empty()).then_some(value)
}

fn sanitize_status_text(value: &str) -> Option<String> {
    let value: String = value
        .trim()
        .chars()
        .filter(|character| !character.is_control() && !is_unicode_format_control(*character))
        .take(MAX_STATUS_TEXT_CHARS)
        .collect();
    (!value.is_empty()).then_some(value)
}

fn is_unicode_format_control(character: char) -> bool {
    matches!(
        character,
        '\u{00ad}'
            | '\u{0600}'..='\u{0605}'
            | '\u{061c}'
            | '\u{06dd}'
            | '\u{070f}'
            | '\u{0890}'..='\u{0891}'
            | '\u{08e2}'
            | '\u{17b4}'..='\u{17b5}'
            | '\u{180e}'
            | '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{206f}'
            | '\u{feff}'
            | '\u{fff9}'..='\u{fffb}'
            | '\u{110bd}'
            | '\u{110cd}'
            | '\u{13430}'..='\u{1343f}'
            | '\u{1bca0}'..='\u{1bca3}'
            | '\u{1d173}'..='\u{1d17a}'
            | '\u{e0001}'
            | '\u{e0020}'..='\u{e007f}'
    )
}

#[cfg(test)]
fn command_output_text(output: &[u8]) -> Option<String> {
    command_output(output).map(|output| output.text)
}

#[derive(Clone, Copy, PartialEq)]
enum ControlSequenceState {
    Text,
    Escape,
    EscapeIntermediate,
    Csi,
    Osc,
    OscEscape,
    StString,
}

fn command_output(value: &[u8]) -> Option<StatusCommandOutput> {
    use ControlSequenceState::*;

    let mut spans = vec![StatusSpan {
        text: String::new(),
        url: None,
    }];
    let mut open_span = None;
    let mut line_break = false;
    let mut osc = String::new();
    let mut state = Text;
    for character in String::from_utf8_lossy(value).chars() {
        if state == OscEscape && character != '\\' {
            osc.clear();
            state = Escape;
        }
        state = match (state, character) {
            (Text, '\x1b') => Escape,
            (Text, _) => {
                append_status_character(&mut spans, &mut open_span, &mut line_break, character);
                Text
            }
            (Escape, '[') => Csi,
            (Escape, ']') => {
                osc.clear();
                Osc
            }
            (Escape, 'P' | 'X' | '^' | '_') => StString,
            (Escape, '\u{20}'..='\u{2f}') => EscapeIntermediate,
            (Escape, '\u{30}'..='\u{7e}') => Text,
            (Escape, '\x1b') => Escape,
            (Escape, '\x18' | '\x1a') => Text,
            (Escape, character) if character.is_ascii_control() => Escape,
            (Escape, _) => {
                append_status_character(&mut spans, &mut open_span, &mut line_break, character);
                Text
            }
            (EscapeIntermediate, '\u{20}'..='\u{2f}') => EscapeIntermediate,
            (EscapeIntermediate, '\u{30}'..='\u{7e}') => Text,
            (EscapeIntermediate, '\x1b') => Escape,
            (EscapeIntermediate, '\x18' | '\x1a') => Text,
            (EscapeIntermediate, character) if character.is_ascii_control() => EscapeIntermediate,
            (EscapeIntermediate, _) => {
                append_status_character(&mut spans, &mut open_span, &mut line_break, character);
                Text
            }
            (Csi, '\u{20}'..='\u{3f}') => Csi,
            (Csi, '\u{40}'..='\u{7e}') => Text,
            (Csi, '\x1b') => Escape,
            (Csi, '\x18' | '\x1a') => Text,
            (Csi, character) if character.is_ascii_control() => Csi,
            (Csi, _) => {
                append_status_character(&mut spans, &mut open_span, &mut line_break, character);
                Text
            }
            (Osc, '\x07') | (OscEscape, '\\') => {
                if let Some(body) = osc.strip_prefix("8;") {
                    let url = body.split_once(';').and_then(|(params, url)| {
                        if params.chars().any(char::is_control) {
                            return None;
                        }
                        crate::protocol::endpoint::status_web_url(url).map(str::to_owned)
                    });
                    open_span = url.as_ref().map(|_| spans.len());
                    spans.push(StatusSpan {
                        text: String::new(),
                        url,
                    });
                }
                osc.clear();
                Text
            }
            (Osc, '\x1b') => OscEscape,
            (Osc, '\x18' | '\x1a') => {
                osc.clear();
                Text
            }
            (Osc, _) => {
                osc.push(character);
                Osc
            }
            (OscEscape, _) => Text,
            (StString, '\x1b') => Escape,
            (StString, '\x18' | '\x1a') => Text,
            (StString, _) => StString,
        };
    }
    // A truncated/unclosed link must not claim the remaining text.
    if let Some(start) = open_span {
        for span in spans.iter_mut().skip(start) {
            span.url = None;
        }
    }
    let start = spans.iter().position(|span| !span.text.trim().is_empty())?;
    let end = spans
        .iter()
        .rposition(|span| !span.text.trim().is_empty())?;
    let mut spans: Vec<_> = spans.drain(start..=end).collect();
    if let Some(first) = spans.first_mut() {
        first.text = first.text.trim_start().to_owned();
    }
    if let Some(last) = spans.last_mut() {
        last.text = last.text.trim_end().to_owned();
    }
    // Command input already has a strict byte bound. Do not truncate a combined
    // result at the old single-label limit, dropping later labels or links.
    spans.retain(|span| !span.text.is_empty());
    let text = spans.iter().map(|span| span.text.as_str()).collect();
    Some(StatusCommandOutput { text, spans })
}

fn append_status_character(
    spans: &mut Vec<StatusSpan>,
    open_span: &mut Option<usize>,
    line_break: &mut bool,
    character: char,
) {
    if character == '\n' {
        if *line_break {
            let url = spans.last().and_then(|span| span.url.clone());
            spans.clear();
            spans.push(StatusSpan {
                text: String::new(),
                url,
            });
            if open_span.is_some() {
                *open_span = Some(0);
            }
        }
        *line_break = true;
        return;
    }
    if character.is_control() || is_unicode_format_control(character) {
        return;
    }
    if *line_break {
        let url = spans.last().and_then(|span| span.url.clone());
        spans.clear();
        spans.push(StatusSpan {
            text: String::new(),
            url,
        });
        if open_span.is_some() {
            *open_span = Some(0);
        }
        *line_break = false;
    }
    if let Some(span) = spans.last_mut() {
        span.text.push(character);
    }
}

async fn read_last_output_line(
    mut stdout: tokio::process::ChildStdout,
) -> std::io::Result<Vec<u8>> {
    let mut current_line = Vec::new();
    let mut last_line = Vec::new();
    let mut ended_with_newline = false;
    let mut buffer = [0_u8; 1024];

    loop {
        let count = stdout.read(&mut buffer).await?;
        if count == 0 {
            break;
        }
        for &byte in &buffer[..count] {
            if byte == b'\n' {
                last_line = std::mem::take(&mut current_line);
                ended_with_newline = true;
            } else {
                if current_line.len() < MAX_COMMAND_LINE_BYTES {
                    current_line.push(byte);
                }
                ended_with_newline = false;
            }
        }
    }

    Ok(if ended_with_newline {
        last_line
    } else {
        current_line
    })
}

struct StatusCommandTask {
    abort_handle: tokio::task::AbortHandle,
    control: Arc<StatusCommandControl>,
}

struct StatusCommandControl {
    terminated: AtomicBool,
    process_group: Mutex<Option<crate::platform::StatusCommandGuard>>,
}

impl StatusCommandControl {
    fn is_terminated(&self) -> bool {
        self.terminated.load(Ordering::Acquire)
    }

    fn terminate(&self) {
        self.terminated.store(true, Ordering::Release);
        if let Some(mut process_group) = self
            .process_group
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        {
            process_group.terminate();
        }
    }

    fn register(&self, mut process_group: crate::platform::StatusCommandGuard) {
        let mut registered = self
            .process_group
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if self.is_terminated() {
            process_group.terminate();
        } else {
            *registered = Some(process_group);
        }
    }
}

fn spawn_status_command(
    event_tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
    generation: u64,
    segment_index: usize,
    command: String,
    timeout: Duration,
    environment: Vec<(String, String)>,
    cwd: Option<std::path::PathBuf>,
) -> StatusCommandTask {
    let control = Arc::new(StatusCommandControl {
        terminated: AtomicBool::new(false),
        process_group: Mutex::new(None),
    });
    let task_control = Arc::clone(&control);
    let deadline = tokio::time::Instant::now() + timeout;
    let task = tokio::spawn(async move {
        let result = run_status_command(
            task_control.as_ref(),
            command,
            timeout,
            deadline,
            environment,
            cwd,
        )
        .await;
        task_control.terminate();
        let _ = event_tx
            .send(crate::events::AppEvent::TabBarCommandFinished {
                generation,
                segment_index,
                result,
            })
            .await;
    });
    StatusCommandTask {
        abort_handle: task.abort_handle(),
        control,
    }
}

async fn run_status_command(
    control: &StatusCommandControl,
    command: String,
    timeout: Duration,
    deadline: tokio::time::Instant,
    environment: Vec<(String, String)>,
    cwd: Option<std::path::PathBuf>,
) -> Result<Option<StatusCommandOutput>, String> {
    if control.is_terminated() || tokio::time::Instant::now() >= deadline {
        return Err(format!("timed out after {}s", timeout.as_secs()));
    }

    let mut process = crate::platform::detached_custom_command_process(&command);
    process
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .envs(environment);
    if let Some(cwd) = cwd {
        process.current_dir(cwd);
    }
    crate::platform::configure_status_command(&mut process);

    let mut process = tokio::process::Command::from(process);
    process.kill_on_drop(true);
    let mut child = process.spawn().map_err(|error| error.to_string())?;
    let process_group =
        crate::platform::StatusCommandGuard::new(&child).map_err(|error| error.to_string())?;
    control.register(process_group);
    if control.is_terminated() {
        return Err("status command was cancelled".into());
    }

    let operation = async {
        let stdout = child.stdout.take();
        let read_output = async {
            let Some(stdout) = stdout else {
                return std::io::Result::Ok(Vec::new());
            };
            read_last_output_line(stdout).await
        };
        let (status, output) = tokio::join!(child.wait(), read_output);
        let status = status.map_err(|error| error.to_string())?;
        let output = output.map_err(|error| error.to_string())?;
        if status.success() {
            Ok(command_output(&output))
        } else {
            Err(format!("exited with {status}"))
        }
    };
    match tokio::time::timeout_at(deadline, operation).await {
        Ok(result) => result,
        Err(_) => Err(format!("timed out after {}s", timeout.as_secs())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::Config, events::AppEvent};

    fn test_app() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        App::new(
            &Config::default(),
            crate::app::AppPolicy::TEST,
            None,
            api_rx,
            crate::api::EventHub::default(),
        )
    }

    #[cfg(unix)]
    const MULTILINE_COMMAND: &str = "printf 'old\\nfinal\\n'";
    #[cfg(windows)]
    const MULTILINE_COMMAND: &str = "echo old & echo final";

    #[cfg(unix)]
    const OVER_CAP_COMMAND: &str = "head -c 5000 /dev/zero | tr '\\0' x; printf '\\nREADY\\n'";

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn unique_temp_path(name: &str) -> std::path::PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock after epoch")
            .as_nanos();
        std::path::PathBuf::from("/var/tmp").join(format!(
            "herdr-tab-status-{name}-{}-{stamp}",
            std::process::id()
        ))
    }

    #[tokio::test]
    async fn status_command_reports_its_sanitized_last_line() {
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(1);
        spawn_status_command(
            event_tx,
            7,
            3,
            MULTILINE_COMMAND.into(),
            Duration::from_secs(2),
            Vec::new(),
            None,
        );

        let event = tokio::time::timeout(Duration::from_secs(3), event_rx.recv())
            .await
            .expect("status command timed out")
            .expect("status command event channel closed");
        assert!(matches!(
            event,
            AppEvent::TabBarCommandFinished {
                generation: 7,
                segment_index: 3,
                result: Ok(Some(ref output)),
            } if output.text == "final" && output.spans.iter().all(|span| span.url.is_none())
        ));
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[tokio::test(flavor = "current_thread")]
    async fn status_command_timeout_starts_before_task_is_polled() {
        let ran = unique_temp_path("ran-after-timeout");
        let command = format!("printf ran > {}", ran.display());
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(1);
        spawn_status_command(
            event_tx,
            7,
            3,
            command,
            Duration::from_secs(1),
            Vec::new(),
            None,
        );

        std::thread::sleep(Duration::from_millis(1100));
        let event = tokio::time::timeout(Duration::from_secs(1), event_rx.recv())
            .await
            .expect("status command timed out")
            .expect("status command event channel closed");
        let command_ran = ran.exists();
        let _ = std::fs::remove_file(ran);
        assert!(matches!(
            event,
            AppEvent::TabBarCommandFinished {
                result: Err(ref error),
                ..
            } if error == "timed out after 1s"
        ));
        assert!(!command_ran, "status command ran after its deadline");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn status_command_drains_large_output_and_keeps_the_last_line() {
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(1);
        spawn_status_command(
            event_tx,
            7,
            3,
            OVER_CAP_COMMAND.into(),
            Duration::from_secs(2),
            Vec::new(),
            None,
        );

        let event = tokio::time::timeout(Duration::from_secs(3), event_rx.recv())
            .await
            .expect("status command timed out")
            .expect("status command event channel closed");
        assert!(matches!(
            event,
            AppEvent::TabBarCommandFinished {
                result: Ok(Some(ref output)),
                ..
            } if output.text == "READY" && output.spans.iter().all(|span| span.url.is_none())
        ));
    }

    #[test]
    fn stale_command_result_does_not_replace_reloaded_status() {
        let mut app = test_app();
        app.configure_tab_bar_status(
            &[TabBarRightEntryConfig::Command {
                command: MULTILINE_COMMAND.into(),
                interval_seconds: 5,
                timeout_seconds: 2,
            }],
            " ",
        );
        let stale_generation = app.tab_bar_status_generation;
        app.configure_tab_bar_status(
            &[TabBarRightEntryConfig::Text {
                text: "fresh".into(),
            }],
            " ",
        );

        app.handle_tab_bar_command_finished(stale_generation, 0, Ok(command_output(b"stale")));

        assert_eq!(
            app.state.tab_bar_right,
            vec![TabBarStatusSegment::Text(Some("fresh".into()))]
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[tokio::test(flavor = "current_thread")]
    async fn reload_aborts_an_in_flight_command_task_and_its_descendants() {
        let descendant_started = unique_temp_path("descendant-started");
        let survived = unique_temp_path("survived");
        let command = format!(
            "(printf descendant-started > {}; sleep 0.3; printf survived > {}) & wait",
            descendant_started.display(),
            survived.display()
        );
        let mut app = test_app();
        app.configure_tab_bar_status(
            &[TabBarRightEntryConfig::Command {
                command,
                interval_seconds: 5,
                timeout_seconds: 20,
            }],
            " ",
        );
        app.handle_tab_bar_status_tasks(std::time::Instant::now());
        for _ in 0..50 {
            if descendant_started.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            descendant_started.exists(),
            "status command descendant did not start"
        );

        app.configure_tab_bar_status(
            &[TabBarRightEntryConfig::Text {
                text: "reloaded".into(),
            }],
            " ",
        );

        // Task cancellation is delivered when Tokio next polls the task. Block
        // this current-thread test runtime long enough for the descendant to
        // run, proving config reload kills its process group synchronously.
        std::thread::sleep(Duration::from_millis(400));
        let descendant_survived = survived.exists();
        let _ = std::fs::remove_file(&descendant_started);
        let _ = std::fs::remove_file(&survived);
        assert!(!descendant_survived, "status command descendant survived");

        assert!(
            tokio::time::timeout(Duration::from_millis(100), app.event_rx.recv())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn in_flight_command_has_no_second_deadline() {
        let mut app = test_app();
        app.configure_tab_bar_status(
            &[TabBarRightEntryConfig::Command {
                command: MULTILINE_COMMAND.into(),
                interval_seconds: 5,
                timeout_seconds: 2,
            }],
            " ",
        );

        let now = std::time::Instant::now();
        assert!(app.next_tab_bar_status_deadline().is_some());
        app.handle_tab_bar_status_tasks(now);

        assert!(app.tab_bar_commands[0].task.is_some());
        assert_eq!(app.next_tab_bar_status_deadline(), None);
    }

    #[test]
    fn datetime_refresh_updates_its_segment_once_per_deadline() {
        let mut app = test_app();
        app.configure_tab_bar_status(
            &[TabBarRightEntryConfig::Datetime {
                format: "%Y-%m-%d %H:%M:%S".into(),
            }],
            " ",
        );
        app.state.tab_bar_right[0] = TabBarStatusSegment::Text(None);
        let deadline = app
            .next_tab_bar_datetime_refresh
            .expect("datetime refresh deadline");

        assert!(app.handle_tab_bar_status_tasks(deadline));
        assert!(matches!(
            &app.state.tab_bar_right[0],
            TabBarStatusSegment::Text(Some(value)) if !value.is_empty()
        ));
        assert!(!app.handle_tab_bar_status_tasks(deadline));
    }

    #[test]
    fn clickable_status_accepts_one_web_link_and_preserves_plain_labels() {
        for end in ["\x07", "\x1b\\"] {
            let raw = format!("\x1b]8;;https://chatgpt.com/codex/cloud/settings/analytics{end}Codex 42%\x1b]8;;{end}");
            let output = command_output(raw.as_bytes()).unwrap();
            assert_eq!(output.text, "Codex 42%");
            assert_eq!(
                output.spans[0].url.as_deref(),
                Some("https://chatgpt.com/codex/cloud/settings/analytics")
            );
        }
        for url in [
            "file:///C:/secret",
            "javascript:alert(1)",
            "https://user:secret@example.test",
            "https://",
            "https://example.test/\0x",
            "https://example.test/a b",
        ] {
            let raw = format!("\x1b]8;;{url}\x07Usage 42%\x1b]8;;\x07");
            let output = command_output(raw.as_bytes()).unwrap();
            assert_eq!(output.text, "Usage 42%");
            assert!(
                output.spans.iter().all(|span| span.url.is_none()),
                "{url:?}"
            );
        }
        assert!(command_output(b"Usage 42%")
            .unwrap()
            .spans
            .iter()
            .all(|span| span.url.is_none()));
        assert!(command_output(b"\x1b]8;;https://example.test\x07Usage 42%")
            .unwrap()
            .spans
            .iter()
            .all(|span| span.url.is_none()));
    }

    #[test]
    fn clickable_status_preserves_multiple_links_and_complete_combined_labels() {
        let entries = [
            ("Codex week: auth unavailable", "https://example.test/codex"),
            (
                "OpenRouter: auth unavailable",
                "https://example.test/router",
            ),
            ("Apify: token unavailable", "https://example.test/apify"),
        ];
        let raw = entries
            .iter()
            .enumerate()
            .map(|(index, (label, url))| {
                let end = if index == 1 { "\x07" } else { "\x1b\\" };
                format!("\x1b]8;;{url}{end}\x1b[32m{label}\x1b[0m\x1b]8;;{end}")
            })
            .collect::<Vec<_>>()
            .join(" | ");
        let parsed = command_output(format!("discard\n  {raw}  \r\n").as_bytes()).unwrap();
        assert_eq!(
            parsed.text,
            entries
                .iter()
                .map(|(label, _)| *label)
                .collect::<Vec<_>>()
                .join(" | ")
        );
        assert_eq!(parsed.text.chars().count(), 86);
        assert_eq!(
            parsed
                .spans
                .iter()
                .filter_map(|span| span.web_url())
                .collect::<Vec<_>>(),
            entries.iter().map(|(_, url)| *url).collect::<Vec<_>>()
        );
        assert_eq!(parsed.spans[1].text, " | ");
        assert!(parsed.spans[1].url.is_none());
        let mixed =
            command_output(b"plain \x1b]8;;https://example.test\x07linked\x1b]8;;\x07 tail")
                .unwrap();
        assert_eq!(mixed.spans.len(), 3);
        assert_eq!(mixed.spans[1].web_url(), Some("https://example.test"));
        let partial = command_output(
            format!("{raw} \x1b]8;;https://example.test/incomplete\x07tail").as_bytes(),
        )
        .unwrap();
        assert_eq!(
            partial
                .spans
                .iter()
                .filter_map(|span| span.web_url())
                .count(),
            3
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn clickable_status_command_preserves_link_from_windows_process() {
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(1);
        spawn_status_command(
            event_tx,
            1,
            0,
            "echo \x1b]8;;https://example.test/usage\x07Usage\x1b]8;;\x07 ^| \x1b]8;;https://example.test/more\x07More\x1b]8;;\x07".into(),
            Duration::from_secs(2),
            Vec::new(),
            None,
        );
        let event = tokio::time::timeout(Duration::from_secs(3), event_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(event, AppEvent::TabBarCommandFinished { result: Ok(Some(ref output)), .. }
            if output.text == "Usage | More" && output.spans.iter().filter_map(|span| span.web_url()).collect::<Vec<_>>() == ["https://example.test/usage", "https://example.test/more"])
        );
    }

    #[test]
    fn clickable_status_refresh_replaces_and_clears_link_without_activation() {
        let mut app = test_app();
        app.configure_tab_bar_status(
            &[TabBarRightEntryConfig::Command {
                command: MULTILINE_COMMAND.into(),
                interval_seconds: 5,
                timeout_seconds: 2,
            }],
            " ",
        );
        let generation = app.tab_bar_status_generation;
        let linked = command_output(b"\x1b]8;;https://example.test\x07Usage\x1b]8;;\x07");
        assert!(app.handle_tab_bar_command_finished(generation, 0, Ok(linked)));
        assert!(
            matches!(&app.state.tab_bar_right[0], TabBarStatusSegment::Link { text, .. } if text == "Usage")
        );
        assert!(app.handle_tab_bar_command_finished(generation, 0, Ok(command_output(b"Usage"))));
        assert_eq!(
            app.state.tab_bar_right[0],
            TabBarStatusSegment::Text(Some("Usage".into()))
        );
    }

    #[test]
    fn command_output_uses_sanitized_last_line() {
        assert_eq!(
            command_output_text(b"old\n win\x1b[31mter\r\n"),
            Some("winter".into())
        );
        assert_eq!(command_output_text(b"\r\n"), None);
    }

    #[test]
    fn command_output_strips_ansi_style_sequences() {
        assert_eq!(
            command_output_text(b"\x1b[32mHELLO\x1b[0m"),
            Some("HELLO".into())
        );
    }

    #[test]
    fn command_output_strips_terminal_control_sequence_families() {
        assert_eq!(
            command_output_text(b"\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\"),
            Some("link".into())
        );
        assert_eq!(
            command_output_text(b"\x1bPignored\x1b\\visible\x1b7"),
            Some("visible".into())
        );
        assert_eq!(command_output_text(b"\x1b[31m\x1b[0m"), None);
        assert_eq!(
            command_output_text(b"\x1b\x07[32mHELLO\x1b[0m"),
            Some("HELLO".into())
        );
        assert_eq!(
            command_output_text(b"\x1bPignored\x18VISIBLE"),
            Some("VISIBLE".into())
        );
        assert_eq!(
            command_output_text(b"\x1bPignored\x1b7VISIBLE"),
            Some("VISIBLE".into())
        );
        assert_eq!(
            command_output_text(b"\x1b]ignored\x1aVISIBLE"),
            Some("VISIBLE".into())
        );
        assert_eq!(command_output_text(b"\xc2\x1b[31m\xa2"), Some("��".into()));

        let styled = format!("\x1b[38;2;1;2;3m{}\x1b[0m", "x".repeat(80));
        assert_eq!(command_output_text(styled.as_bytes()), Some("x".repeat(80)));
    }

    #[test]
    fn status_text_strips_bidi_and_zero_width_format_controls() {
        assert_eq!(
            sanitize_status_text("safe\u{202e}evil\u{200b}"),
            Some("safeevil".into())
        );
    }

    #[test]
    fn separator_preserves_printable_spacing_and_drops_controls() {
        assert_eq!(sanitize_separator(" \x1b|\n "), " | ");
    }
}
