using System.Collections.ObjectModel;
using System.Diagnostics;
using System.Globalization;
using System.Text.Json;
using System.Runtime.InteropServices;
using Microsoft.UI;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Windows.ApplicationModel;
using WinRT.Interop;

namespace OpenShazam.Settings;

public sealed partial class MainWindow : Window
{
    internal const string WindowTitle = "OpenShazam";
    private const string StartupTaskId = "OpenShazamStartup";
    private const double SettingsWidthDips = 620;
    private const double SettingsHeightDips = 800;
    private const double DipsPerInch = 96;
    private StartupTask? _startupTask;
    private bool _isLoading;
    private readonly bool _isJapanese;
    private readonly AppWindow _appWindow;
    private XamlRoot? _observedXamlRoot;
    private double _lastRasterizationScale;
    private readonly ObservableCollection<HistoryTrack> _history = new();
    private readonly SemaphoreSlim _historyWriteGate = new(1, 1);
    private RecognitionBridge? _bridge;
    private string? _currentPageUrl;
    private bool _isRunning;

    public MainWindow()
    {
        InitializeComponent();
        Title = WindowTitle;
        _isJapanese = CultureInfo.CurrentUICulture.TwoLetterISOLanguageName.Equals("ja", StringComparison.OrdinalIgnoreCase);
        PageDescription.Text = Text(
            "Music recognition powered by the native Rust engine.",
            "Rust製のネイティブ認識エンジンで楽曲を認識します。");
        StatusText.Text = Text("Ready to recognize", "認識の準備ができました");
        RecognizeButton.Content = Text("Recognize music", "楽曲を認識");
        AutoModeToggle.Header = Text("Continuous recognition", "自動認識");
        HistoryTitle.Text = Text("History", "履歴");
        SettingsExpander.Header = Text("Settings", "設定");
        SettingsDescription.Text = Text(
            "Configure OpenShazam startup behavior.",
            "OpenShazamの起動設定を管理します。");
        StartupToggle.Header = Text(
            "Launch OpenShazam when I sign in",
            "Windowsへのサインイン時にOpenShazamを起動");
        StartupToggle.OffContent = Text("Off", "オフ");
        StartupToggle.OnContent = Text("On", "オン");
        StartupDescription.Text = Text(
            "OpenShazam will start without automatically listening for music.",
            "OpenShazamの起動時に楽曲認識は自動開始されません。");
        HistoryList.ItemsSource = _history;
        LoadHistory();
        Closed += OnWindowClosed;
        Root.Loaded += OnLoaded;

        try
        {
            _bridge = new RecognitionBridge(
                message => DispatcherQueue.TryEnqueue(() => HandleBridgeEvent(message)),
                message => DispatcherQueue.TryEnqueue(() =>
                {
                    SetRunning(false);
                    ShowInfo(message, InfoBarSeverity.Error);
                }));
        }
        catch (Exception error) when (error is IOException or InvalidOperationException)
        {
            ShowInfo(Text(
                $"Recognition service unavailable: {error.Message}",
                $"認識サービスを起動できません: {error.Message}"),
                InfoBarSeverity.Error);
            RecognizeButton.IsEnabled = false;
            AutoModeToggle.IsEnabled = false;
        }

        var hwnd = WindowNative.GetWindowHandle(this);
        var windowId = Win32Interop.GetWindowIdFromWindow(hwnd);
        // AppWindow.Resize takes physical pixels; XAML layout dimensions are DIPs.
        // Size from the actual window monitor to avoid clipping at high DPI.
        var windowDpi = NativeMethods.GetDpiForWindow(hwnd);
        _lastRasterizationScale = (windowDpi == 0 ? DipsPerInch : windowDpi) / DipsPerInch;
        _appWindow = AppWindow.GetFromWindowId(windowId);
        ResizeForRasterizationScale(_lastRasterizationScale);
    }

    private async void OnLoaded(object sender, RoutedEventArgs args)
    {
        // Unlike UWP, WinUI 3 desktop windows must observe the XAML root's
        // rasterization scale for per-monitor DPI changes.
        if (Root.XamlRoot is { } xamlRoot && !ReferenceEquals(xamlRoot, _observedXamlRoot))
        {
            if (_observedXamlRoot is not null)
            {
                _observedXamlRoot.Changed -= OnXamlRootChanged;
            }

            _observedXamlRoot = xamlRoot;
            xamlRoot.Changed += OnXamlRootChanged;
            ApplyRasterizationScale(xamlRoot.RasterizationScale);
        }

        await RefreshStartupStateAsync();
    }

    private void OnXamlRootChanged(XamlRoot sender, XamlRootChangedEventArgs args)
    {
        ApplyRasterizationScale(sender.RasterizationScale);
    }

    private void ApplyRasterizationScale(double scale)
    {
        if (double.IsFinite(scale) && scale > 0 && Math.Abs(scale - _lastRasterizationScale) > 0.001)
        {
            _lastRasterizationScale = scale;
            ResizeForRasterizationScale(scale);
        }
    }

    private void ResizeForRasterizationScale(double scale)
    {
        // AppWindow uses physical pixels; the XAML panel uses effective pixels.
        // Keep the intended logical size when there is room, but never
        // exceed the active monitor's work area (including taskbar bounds).
        // The XAML content is scrollable on compact and portrait displays.
        var workArea = DisplayArea.GetFromWindowId(_appWindow.Id, DisplayAreaFallback.Nearest).WorkArea;
        var desiredWidth = (int)Math.Ceiling(SettingsWidthDips * scale);
        var desiredHeight = (int)Math.Ceiling(SettingsHeightDips * scale);
        var width = workArea.Width > 0 ? Math.Min(desiredWidth, workArea.Width) : desiredWidth;
        var height = workArea.Height > 0 ? Math.Min(desiredHeight, workArea.Height) : desiredHeight;
        if (_appWindow.Size.Width != width || _appWindow.Size.Height != height)
        {
            _appWindow.Resize(new Windows.Graphics.SizeInt32(width, height));
        }
    }

    private void SetRunning(bool value)
    {
        _isRunning = value;
        ListeningRing.IsActive = value;
        RecognizeButton.Content = value
            ? Text("Stop recognition", "認識を停止")
            : Text("Recognize music", "楽曲を認識");
    }

    private async void OnRecognizeClicked(object sender, RoutedEventArgs args)
    {
        if (_bridge is null) return;
        try
        {
            await _bridge.SendAsync(_isRunning ? "cancel" : "start_manual");
            if (!_isRunning)
            {
                SetRunning(true);
                StatusText.Text = Text("Preparing recognition…", "認識を準備しています…");
            }
        }
        catch (Exception error) when (error is IOException or InvalidOperationException)
        {
            ShowInfo(error.Message, InfoBarSeverity.Error);
            SetRunning(false);
        }
    }

    private async void OnAutoToggled(object sender, RoutedEventArgs args)
    {
        if (_bridge is null) return;
        try
        {
            if (AutoModeToggle.IsOn && !_isRunning)
            {
                await _bridge.SendAsync("start_auto");
                SetRunning(true);
            }
            else if (!AutoModeToggle.IsOn && _isRunning)
            {
                await _bridge.SendAsync("cancel");
            }
        }
        catch (Exception error) when (error is IOException or InvalidOperationException)
        {
            ShowInfo(error.Message, InfoBarSeverity.Error);
        }
    }

    private void HandleBridgeEvent(JsonElement message)
    {
        if (!message.TryGetProperty("kind", out var kind)) return;
        switch (kind.GetString())
        {
            case "ready":
                StatusText.Text = Text("Ready to recognize", "認識の準備ができました");
                break;
            case "status":
                var state = ReadString(message, "value");
                switch (state)
                {
                    case "listening":
                        SetRunning(true);
                        StatusText.Text = Text("Listening…", "音楽を聴き取っています…");
                        break;
                    case "no_match":
                        StatusText.Text = Text("No match found", "一致する楽曲が見つかりませんでした");
                        break;
                    case "timeout":
                        StatusText.Text = Text("Recognition timed out", "認識がタイムアウトしました");
                        break;
                }
                break;
            case "match":
                var track = new HistoryTrack
                {
                    Title = ReadString(message, "title"),
                    Artist = ReadString(message, "artist"),
                    RecognizedAtUnixMs = ReadInt64(message, "recognizedAtUnixMs"),
                    PageUrl = SafeShazamUrl(ReadString(message, "pageUrl")),
                };
                ResultCard.Visibility = Visibility.Visible;
                ResultTitle.Text = track.Title;
                ResultArtist.Text = track.Artist;
                _currentPageUrl = track.PageUrl;
                OpenSongButton.IsEnabled = track.PageUrl is not null;
                StatusText.Text = Text("Recognized!", "認識しました！");
                _history.Insert(0, track);
                while (_history.Count > 40) _history.RemoveAt(_history.Count - 1);
                HistoryCount.Text = _history.Count.ToString(CultureInfo.CurrentCulture);
                _ = SaveHistoryAsync(_history.ToArray());
                break;
            case "ended":
                SetRunning(false);
                if (AutoModeToggle.IsOn) AutoModeToggle.IsOn = false;
                break;
            case "error":
                SetRunning(false);
                StatusText.Text = Text("Recognition error", "認識エラー");
                ShowInfo(ReadString(message, "message"), InfoBarSeverity.Error);
                break;
        }
    }

    private static string ReadString(JsonElement value, string key) =>
        value.TryGetProperty(key, out var property) &&
        property.ValueKind == JsonValueKind.String ? property.GetString() ?? "" : "";

    private static long ReadInt64(JsonElement value, string key) =>
        value.TryGetProperty(key, out var property) &&
        property.ValueKind == JsonValueKind.Number &&
        property.TryGetInt64(out var timestamp) ? timestamp : 0;

    private static string? SafeShazamUrl(string? value)
    {
        if (!Uri.TryCreate(value, UriKind.Absolute, out var url) ||
            url.Scheme != Uri.UriSchemeHttps ||
            url.Port != 443 ||
            !string.IsNullOrEmpty(url.UserInfo) ||
            !string.IsNullOrEmpty(url.Fragment) ||
            !(url.Host.Equals("shazam.com", StringComparison.OrdinalIgnoreCase) ||
              url.Host.EndsWith(".shazam.com", StringComparison.OrdinalIgnoreCase)))
            return null;
        return url.AbsoluteUri;
    }

    private void OnOpenSongClicked(object sender, RoutedEventArgs args)
    {
        OpenShazamUrl(_currentPageUrl);
    }

    private void OnHistoryItemClick(object sender, ItemClickEventArgs args)
    {
        if (args.ClickedItem is HistoryTrack track)
            OpenShazamUrl(track.PageUrl);
    }

    private void OpenShazamUrl(string? value)
    {
        var safe = SafeShazamUrl(value);
        if (safe is null)
        {
            ShowInfo(Text("No verified Shazam song link is available.",
                          "確認済みのShazam楽曲リンクがありません。"));
            return;
        }
        try
        {
            Process.Start(new ProcessStartInfo(safe) { UseShellExecute = true });
        }
        catch (Exception error) when (error is InvalidOperationException or System.ComponentModel.Win32Exception)
        {
            ShowInfo(error.Message, InfoBarSeverity.Error);
        }
    }

    private static string HistoryPath => Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
        "OpenShazam", "history.json");

    private void LoadHistory()
    {
        try
        {
            if (File.Exists(HistoryPath))
            {
                var tracks = JsonSerializer.Deserialize<HistoryTrack[]>(File.ReadAllText(HistoryPath));
                foreach (var track in tracks?.Take(40) ?? Array.Empty<HistoryTrack>())
                {
                    track.PageUrl = SafeShazamUrl(track.PageUrl);
                    _history.Add(track);
                }
            }
        }
        catch (Exception error) when (error is IOException or JsonException or UnauthorizedAccessException)
        {
            ShowInfo(Text($"Could not load history: {error.Message}",
                          $"履歴を読み込めません: {error.Message}"), InfoBarSeverity.Warning);
        }
        HistoryCount.Text = _history.Count.ToString(CultureInfo.CurrentCulture);
    }

    private async Task SaveHistoryAsync(HistoryTrack[] snapshot)
    {
        await _historyWriteGate.WaitAsync();
        try
        {
            var path = HistoryPath;
            Directory.CreateDirectory(Path.GetDirectoryName(path)!);
            var temp = path + ".tmp";
            await File.WriteAllTextAsync(temp, JsonSerializer.Serialize(snapshot));
            File.Move(temp, path, overwrite: true);
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException)
        {
            DispatcherQueue.TryEnqueue(() => ShowInfo(error.Message, InfoBarSeverity.Warning));
        }
        finally
        {
            _historyWriteGate.Release();
        }
    }

    private async void OnWindowClosed(object sender, WindowEventArgs args)
    {
        if (_bridge is not null)
            await _bridge.DisposeAsync();
    }

    private async void OnStartupToggled(object sender, RoutedEventArgs args)
    {
        if (_isLoading || _startupTask is null)
        {
            return;
        }

        StartupToggle.IsEnabled = false;
        StatusInfo.IsOpen = false;
        try
        {
            if (StartupToggle.IsOn)
            {
                _ = await _startupTask.RequestEnableAsync();
            }
            else
            {
                _startupTask.Disable();
            }
            await RefreshStartupStateAsync();
        }
        catch (Exception error)
        {
            ShowInfo(Text(
                $"Windows could not update the startup setting: {error.Message}",
                $"自動起動の設定を変更できませんでした: {error.Message}"),
                InfoBarSeverity.Error);
            await RefreshStartupStateAsync();
        }
    }

    private async Task RefreshStartupStateAsync()
    {
        _isLoading = true;
        StartupToggle.IsEnabled = false;
        try
        {
            _startupTask = await StartupTask.GetAsync(StartupTaskId);
            var state = _startupTask.State;
            StartupToggle.IsOn = state is StartupTaskState.Enabled or StartupTaskState.EnabledByPolicy;
            StartupToggle.IsEnabled = state is StartupTaskState.Disabled or StartupTaskState.Enabled;

            switch (state)
            {
                case StartupTaskState.DisabledByUser:
                    ShowInfo(Text(
                        "Startup was disabled in Windows. Turn it on in Settings or Task Manager to re-enable it.",
                        "Windows側で自動起動が無効になっています。再度有効にするにはWindowsの設定またはタスクマネージャーを使用してください。"));
                    break;
                case StartupTaskState.DisabledByPolicy:
                    ShowInfo(Text(
                        "Startup is disabled by your organization's policy.",
                        "組織のポリシーによって自動起動が無効になっています。"));
                    break;
                case StartupTaskState.EnabledByPolicy:
                    ShowInfo(Text(
                        "Startup is enabled by your organization's policy.",
                        "組織のポリシーによって自動起動が有効になっています。"));
                    break;
                default:
                    StatusInfo.IsOpen = false;
                    break;
            }
        }
        catch (Exception)
        {
            _startupTask = null;
            StartupToggle.IsOn = false;
            StartupToggle.IsEnabled = false;
            ShowInfo(Text(
                "Automatic startup is available in the packaged version of OpenShazam.",
                "自動起動はパッケージ版のOpenShazamで利用できます。"));
        }
        finally
        {
            _isLoading = false;
        }
    }

    private static class NativeMethods
    {
        [DllImport("user32.dll")]
        internal static extern uint GetDpiForWindow(IntPtr hwnd);
    }

    private string Text(string english, string japanese) => _isJapanese ? japanese : english;

    private void ShowInfo(string message, InfoBarSeverity severity = InfoBarSeverity.Informational)
    {
        StatusInfo.Severity = severity;
        StatusInfo.Message = message;
        StatusInfo.IsOpen = true;
    }
}
