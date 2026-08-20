using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Text;

namespace HerdrWin.Probes
{
    public static class KillOnCloseProcess
    {
        private const uint CreateSuspended = 0x00000004;
        private const uint JobObjectExtendedLimitInformation = 9;
        private const uint JobObjectLimitKillOnJobClose = 0x00002000;
        private const uint WaitObject0 = 0x00000000;
        private const uint WaitTimeout = 0x00000102;
        private const uint InfiniteResumeFailure = 0xffffffff;

        [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
        private struct StartupInfo
        {
            public int Size;
            public string Reserved;
            public string Desktop;
            public string Title;
            public uint X;
            public uint Y;
            public uint XSize;
            public uint YSize;
            public uint XCountChars;
            public uint YCountChars;
            public uint FillAttribute;
            public uint Flags;
            public ushort ShowWindow;
            public ushort Reserved2Size;
            public IntPtr Reserved2;
            public IntPtr StandardInput;
            public IntPtr StandardOutput;
            public IntPtr StandardError;
        }

        [StructLayout(LayoutKind.Sequential)]
        private struct ProcessInformation
        {
            public IntPtr Process;
            public IntPtr Thread;
            public uint ProcessId;
            public uint ThreadId;
        }

        [StructLayout(LayoutKind.Sequential)]
        private struct BasicLimitInformation
        {
            public long PerProcessUserTimeLimit;
            public long PerJobUserTimeLimit;
            public uint LimitFlags;
            public UIntPtr MinimumWorkingSetSize;
            public UIntPtr MaximumWorkingSetSize;
            public uint ActiveProcessLimit;
            public UIntPtr Affinity;
            public uint PriorityClass;
            public uint SchedulingClass;
        }

        [StructLayout(LayoutKind.Sequential)]
        private struct IoCounters
        {
            public ulong ReadOperationCount;
            public ulong WriteOperationCount;
            public ulong OtherOperationCount;
            public ulong ReadTransferCount;
            public ulong WriteTransferCount;
            public ulong OtherTransferCount;
        }

        [StructLayout(LayoutKind.Sequential)]
        private struct ExtendedLimitInformation
        {
            public BasicLimitInformation BasicLimitInformation;
            public IoCounters IoInfo;
            public UIntPtr ProcessMemoryLimit;
            public UIntPtr JobMemoryLimit;
            public UIntPtr PeakProcessMemoryUsed;
            public UIntPtr PeakJobMemoryUsed;
        }

        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern IntPtr CreateJobObject(IntPtr attributes, string name);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool SetInformationJobObject(
            IntPtr job,
            uint informationClass,
            IntPtr information,
            uint informationLength);

        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern bool CreateProcess(
            string applicationName,
            StringBuilder commandLine,
            IntPtr processAttributes,
            IntPtr threadAttributes,
            bool inheritHandles,
            uint creationFlags,
            IntPtr environment,
            string currentDirectory,
            ref StartupInfo startupInfo,
            out ProcessInformation processInformation);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool AssignProcessToJobObject(IntPtr job, IntPtr process);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern uint ResumeThread(IntPtr thread);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern uint WaitForSingleObject(IntPtr handle, uint milliseconds);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool GetExitCodeProcess(IntPtr process, out uint exitCode);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool TerminateProcess(IntPtr process, uint exitCode);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool TerminateJobObject(IntPtr job, uint exitCode);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool CloseHandle(IntPtr handle);

        public static int Run(
            string executable,
            string arguments,
            string workingDirectory,
            int timeoutMilliseconds)
        {
            if (string.IsNullOrWhiteSpace(executable) ||
                string.IsNullOrWhiteSpace(workingDirectory) ||
                timeoutMilliseconds <= 0)
            {
                throw new ArgumentException("probe process arguments are invalid");
            }

            IntPtr job = CreateJobObject(IntPtr.Zero, null);
            if (job == IntPtr.Zero)
            {
                throw new Win32Exception(Marshal.GetLastWin32Error(), "CreateJobObject failed");
            }

            ProcessInformation process = new ProcessInformation();
            try
            {
                ExtendedLimitInformation limits = new ExtendedLimitInformation();
                limits.BasicLimitInformation.LimitFlags = JobObjectLimitKillOnJobClose;
                int limitsSize = Marshal.SizeOf(typeof(ExtendedLimitInformation));
                IntPtr limitsPointer = Marshal.AllocHGlobal(limitsSize);
                try
                {
                    Marshal.StructureToPtr(limits, limitsPointer, false);
                    if (!SetInformationJobObject(
                        job,
                        JobObjectExtendedLimitInformation,
                        limitsPointer,
                        (uint)limitsSize))
                    {
                        throw new Win32Exception(
                            Marshal.GetLastWin32Error(),
                            "SetInformationJobObject failed");
                    }
                }
                finally
                {
                    Marshal.FreeHGlobal(limitsPointer);
                }

                StartupInfo startup = new StartupInfo();
                startup.Size = Marshal.SizeOf(typeof(StartupInfo));
                StringBuilder commandLine = new StringBuilder(
                    QuoteArgument(executable) + " " + arguments);
                if (!CreateProcess(
                    executable,
                    commandLine,
                    IntPtr.Zero,
                    IntPtr.Zero,
                    false,
                    CreateSuspended,
                    IntPtr.Zero,
                    workingDirectory,
                    ref startup,
                    out process))
                {
                    throw new Win32Exception(Marshal.GetLastWin32Error(), "CreateProcess failed");
                }
                if (!AssignProcessToJobObject(job, process.Process))
                {
                    int assignError = Marshal.GetLastWin32Error();
                    TerminateProcess(process.Process, 1);
                    WaitForSingleObject(process.Process, 5000);
                    throw new Win32Exception(
                        assignError,
                        "AssignProcessToJobObject failed");
                }
                if (ResumeThread(process.Thread) == InfiniteResumeFailure)
                {
                    throw new Win32Exception(Marshal.GetLastWin32Error(), "ResumeThread failed");
                }

                uint wait = WaitForSingleObject(process.Process, (uint)timeoutMilliseconds);
                if (wait == WaitTimeout)
                {
                    TerminateJobObject(job, 1);
                    WaitForSingleObject(process.Process, 5000);
                    throw new TimeoutException("probe process exceeded its deadline");
                }
                if (wait != WaitObject0)
                {
                    throw new Win32Exception(Marshal.GetLastWin32Error(), "process wait failed");
                }

                uint exitCode;
                if (!GetExitCodeProcess(process.Process, out exitCode))
                {
                    throw new Win32Exception(Marshal.GetLastWin32Error(), "GetExitCodeProcess failed");
                }
                return unchecked((int)exitCode);
            }
            finally
            {
                if (process.Thread != IntPtr.Zero)
                {
                    CloseHandle(process.Thread);
                }
                if (process.Process != IntPtr.Zero)
                {
                    CloseHandle(process.Process);
                }
                CloseHandle(job);
            }
        }

        private static string QuoteArgument(string value)
        {
            return "\"" + value.Replace("\"", "\\\"") + "\"";
        }
    }
}
