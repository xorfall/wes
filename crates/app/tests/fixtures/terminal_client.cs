using System;
using System.Diagnostics;
using System.IO;
using System.Linq;
using System.Runtime.InteropServices;
using System.Threading;

class TerminalClient
{
    [DllImport("kernel32.dll")]
    static extern uint GetConsoleProcessList(uint[] processes, uint count);

    static string Json(string text)
    {
        return "\"" + text.Replace("\\", "\\\\").Replace("\"", "\\\"")
            .Replace("\r", "\\r").Replace("\n", "\\n") + "\"";
    }

    static int Main(string[] args)
    {
        // A failed regression assertion cannot leave a separate interactive console alive.
        new Thread(() => { Thread.Sleep(15000); Environment.Exit(99); })
            { IsBackground = true }.Start();
        var interrupted = new ManualResetEvent(false);
        Console.CancelKeyPress += (sender, request) => { request.Cancel = true; interrupted.Set(); };
        var processes = new uint[64];
        var count = GetConsoleProcessList(processes, (uint)processes.Length);
        var report = Environment.GetEnvironmentVariable("WES_TEST_CONSOLE");
        File.WriteAllText(report, "{\"pid\":" + Process.GetCurrentProcess().Id + ",\"console\":[" + String.Join(",", processes.Take((int)count))
            + "],\"inputRedirected\":" + Console.IsInputRedirected.ToString().ToLowerInvariant()
            + ",\"outputRedirected\":" + Console.IsOutputRedirected.ToString().ToLowerInvariant()
            + ",\"args\":" + Json(String.Join(" ", args))
            + ",\"config\":" + Json(Environment.GetEnvironmentVariable("OPENCODE_CONFIG_CONTENT") ?? "")
            + "}|END");
        Console.WriteLine("CLIENT READY");
        var waiting = args.Contains("--wait-for-interrupt");
        if (waiting) interrupted.WaitOne();
        var input = waiting ? "<interrupt>" : Console.ReadLine();
        File.WriteAllText(Path.ChangeExtension(report, "input"), (input ?? "<eof>") + "|END");
        Console.WriteLine("CLIENT INPUT " + input);
        return waiting ? 31 : 23;
    }
}
