using System.Diagnostics;
using System.Text.Json;

namespace OpenShazam.Settings;

/// <summary>
/// One bounded, line-oriented IPC connection to the existing Rust controller.
/// Audio processing never runs in the WinUI dispatcher or the .NET process.
/// </summary>
internal sealed class RecognitionBridge : IAsyncDisposable
{
    private readonly Process _process;
    private readonly SemaphoreSlim _writeGate = new(1, 1);
    private readonly Task _readerTask;
    private readonly Task _errorTask;

    public event Action<JsonElement>? EventReceived;
    public event Action<string>? Disconnected;

    public RecognitionBridge()
    {
        var executable = Path.Combine(AppContext.BaseDirectory, "openshazam-ui-bridge.exe");
        if (!File.Exists(executable))
        {
            throw new FileNotFoundException(
                "The Rust recognition bridge is missing from the application directory.",
                executable);
        }

        _process = Process.Start(new ProcessStartInfo
        {
            FileName = executable,
            WorkingDirectory = AppContext.BaseDirectory,
            UseShellExecute = false,
            RedirectStandardInput = true,
            RedirectStandardOutput = true,
            RedirectStandardError = true,
            CreateNoWindow = true,
        }) ?? throw new InvalidOperationException("Could not start Rust recognition bridge.");

        _readerTask = Task.Run(ReadEventsAsync);
        // Always drain diagnostics so Rust cannot block if its stderr pipe fills.
        _errorTask = Task.Run(async () =>
        {
            try
            {
                while (await _process.StandardError.ReadLineAsync() is not null) { }
            }
            catch (IOException) { }
        });
    }

    private async Task ReadEventsAsync()
    {
        try
        {
            while (await _process.StandardOutput.ReadLineAsync() is { } line)
            {
                if (line.Length > 262144)
                    throw new InvalidDataException("Recognition event exceeded the IPC limit.");

                using var document = JsonDocument.Parse(line);
                var value = document.RootElement;
                if (value.ValueKind == JsonValueKind.Object)
                    EventReceived?.Invoke(value.Clone());
            }
            Disconnected?.Invoke("Recognition service has exited.");
        }
        catch (Exception error) when (error is IOException or JsonException or InvalidDataException)
        {
            Disconnected?.Invoke($"Recognition connection closed: {error.Message}");
        }
    }

    public async Task SendAsync(string command)
    {
        await _writeGate.WaitAsync();
        try
        {
            var payload = JsonSerializer.Serialize(new { command });
            await _process.StandardInput.WriteLineAsync(payload);
            await _process.StandardInput.FlushAsync();
        }
        finally
        {
            _writeGate.Release();
        }
    }

    public async ValueTask DisposeAsync()
    {
        try
        {
            if (!_process.HasExited)
            {
                await SendAsync("shutdown");
                using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(3));
                try
                {
                    await _process.WaitForExitAsync(deadline.Token);
                }
                catch (OperationCanceledException)
                {
                    _process.Kill(entireProcessTree: true);
                    await _process.WaitForExitAsync();
                }
            }
        }
        catch (InvalidOperationException) { }
        catch (IOException) { }
        finally
        {
            _process.Dispose();
            _writeGate.Dispose();
        }
    }
}
