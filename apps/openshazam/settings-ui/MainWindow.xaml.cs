using System.Globalization;
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
    internal const string WindowTitle = "OpenShazam Settings";
    private const string StartupTaskId = "OpenShazamStartup";
    private const double SettingsWidthDips = 520;
    private const double SettingsHeightDips = 300;
    private const double DipsPerInch = 96;
    private StartupTask? _startupTask;
    private bool _isLoading;
    private readonly bool _isJapanese;
    private readonly AppWindow _appWindow;
    private XamlRoot? _observedXamlRoot;
    private double _lastRasterizationScale;

    public MainWindow()
    {
        InitializeComponent();
        Title = WindowTitle;
        _isJapanese = CultureInfo.CurrentUICulture.TwoLetterISOLanguageName.Equals("ja", StringComparison.OrdinalIgnoreCase);
        PageTitle.Text = Text("Settings", "設定");
        PageDescription.Text = Text(
            "Manage how OpenShazam starts and behaves.",
            "OpenShazamの起動方法を設定します。");
        StartupToggle.Header = Text(
            "Launch OpenShazam when I sign in",
            "Windowsへのサインイン時にOpenShazamを起動");
        StartupToggle.OffContent = Text("Off", "オフ");
        StartupToggle.OnContent = Text("On", "オン");
        StartupDescription.Text = Text(
            "OpenShazam starts in the notification area. Music recognition will not start automatically.",
            "OpenShazamは通知領域で起動します。楽曲認識は自動で開始しません。");
        Root.Loaded += OnLoaded;

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
            OnXamlRootChanged(xamlRoot, null!);
        }

        await RefreshStartupStateAsync();
    }

    private void OnXamlRootChanged(XamlRoot sender, XamlRootChangedEventArgs args)
    {
        var scale = sender.RasterizationScale;
        if (double.IsFinite(scale) && scale > 0 && Math.Abs(scale - _lastRasterizationScale) > 0.001)
        {
            _lastRasterizationScale = scale;
            ResizeForRasterizationScale(scale);
        }
    }

    private void ResizeForRasterizationScale(double scale)
    {
        // AppWindow uses physical pixels; the XAML panel uses effective pixels.
        // Match the same 520×300 logical window footprint at every monitor DPI.
        var width = (int)Math.Ceiling(SettingsWidthDips * scale);
        var height = (int)Math.Ceiling(SettingsHeightDips * scale);
        if (_appWindow.Size.Width != width || _appWindow.Size.Height != height)
        {
            _appWindow.Resize(new Windows.Graphics.SizeInt32(width, height));
        }
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
