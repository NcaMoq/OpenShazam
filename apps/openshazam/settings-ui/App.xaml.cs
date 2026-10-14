using System.Runtime.InteropServices;
using Microsoft.UI.Xaml;

namespace OpenShazam.Settings;

public partial class App : Application
{
    private static readonly Mutex SingleInstanceMutex = AcquireSingleInstance();
    private MainWindow? _window;

    public App()
    {
        InitializeComponent();
    }

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        _window = new MainWindow();
        _window.Activate();
    }

    private static Mutex AcquireSingleInstance()
    {
        var mutex = new Mutex(initiallyOwned: true, "Local\\OpenShazam.Settings.SingleInstance", out var createdNew);
        if (!createdNew)
        {
            var existing = NativeMethods.FindWindow(null, MainWindow.WindowTitle);
            if (existing != IntPtr.Zero)
            {
                _ = NativeMethods.SetForegroundWindow(existing);
            }
            Environment.Exit(0);
        }
        return mutex;
    }

    private static class NativeMethods
    {
        [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        internal static extern IntPtr FindWindow(string? className, string windowName);

        [DllImport("user32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        internal static extern bool SetForegroundWindow(IntPtr hwnd);
    }
}
