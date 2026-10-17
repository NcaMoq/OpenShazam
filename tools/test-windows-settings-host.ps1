param(
    [string]$Directory = (Join-Path (Split-Path -Parent $PSScriptRoot) 'dist\windows-x64')
)

$ErrorActionPreference = 'Stop'

$dist = (Resolve-Path -LiteralPath $Directory -ErrorAction Stop).Path
foreach ($name in @('openshazam.exe', 'OpenShazam.SettingsHost.dll', 'Microsoft.WindowsAppRuntime.Bootstrap.dll')) {
    $path = Join-Path $dist $name
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        throw "Missing $path. Run tools/build-windows.ps1 first."
    }
}

# Run on an isolated, non-visible desktop. This checks the same STA,
# DispatcherQueue, Windows.UI.Composition, Windows App SDK bootstrap, and
# WinUI XAML host initialization order without interacting with the user's UI.
$source = @'
using System;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;
using System.Drawing;
using System.Drawing.Imaging;
using System.Windows.Automation;
using Microsoft.Win32;

public static class OpenShazamSettingsHostSmoke
{
    [StructLayout(LayoutKind.Sequential)]
    public struct DispatcherQueueOptions
    {
        public uint Size;
        public uint ThreadType;
        public uint ApartmentType;
    }

    [StructLayout(LayoutKind.Sequential)]
    public struct Message
    {
        public IntPtr Window;
        public uint Id;
        public UIntPtr WParam;
        public IntPtr LParam;
        public uint Time;
        public int X;
        public int Y;
    }

    [StructLayout(LayoutKind.Sequential)]
    public struct Rect
    {
        public int Left;
        public int Top;
        public int Right;
        public int Bottom;
    }

    [UnmanagedFunctionPointer(CallingConvention.StdCall)]
    private delegate int BootstrapInitialize(uint release, [MarshalAs(UnmanagedType.LPWStr)] string tag, ulong minVersion);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)]
    private delegate void BootstrapShutdown();
    [UnmanagedFunctionPointer(CallingConvention.StdCall)]
    private delegate int SettingsShow(IntPtr owner, uint appearanceMode);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)]
    private delegate void SettingsShutdown();

    [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern IntPtr CreateDesktopW(string name, IntPtr device, IntPtr mode, uint flags, uint access, IntPtr security);
    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool SetThreadDesktop(IntPtr desktop);
    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool CloseDesktop(IntPtr desktop);
    [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern IntPtr CreateWindowExW(uint exStyle, string className, string title, uint style, int x, int y, int width, int height, IntPtr parent, IntPtr menu, IntPtr instance, IntPtr parameter);
    [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern IntPtr FindWindowW(string className, string windowName);
    [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern IntPtr FindWindowExW(IntPtr parent, IntPtr childAfter, IntPtr className, IntPtr windowName);
    [DllImport("user32.dll", SetLastError = true)]
    private static extern IntPtr GetParent(IntPtr window);
    [DllImport("user32.dll", EntryPoint = "GetWindowLongPtrW", SetLastError = true)]
    private static extern IntPtr GetWindowLongPtrW(IntPtr window, int index);
    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool IsWindowVisible(IntPtr window);
    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool GetWindowRect(IntPtr window, out Rect rect);
    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool PrintWindow(IntPtr window, IntPtr deviceContext, uint flags);
    [DllImport("dwmapi.dll", SetLastError = true)]
    private static extern int DwmGetWindowAttribute(IntPtr window, int attribute, out int value, uint size);
    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool DestroyWindow(IntPtr window);
    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool PeekMessageW(out Message message, IntPtr window, uint min, uint max, uint remove);
    [DllImport("user32.dll")]
    private static extern bool TranslateMessage(ref Message message);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern IntPtr DispatchMessageW(ref Message message);

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern IntPtr LoadLibraryW(string path);
    [DllImport("kernel32.dll", CharSet = CharSet.Ansi, SetLastError = true)]
    private static extern IntPtr GetProcAddress(IntPtr module, string name);
    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool FreeLibrary(IntPtr module);
    [DllImport("ole32.dll")]
    private static extern int CoInitializeEx(IntPtr reserved, uint apartment);
    [DllImport("ole32.dll")]
    private static extern void CoUninitialize();
    [DllImport("combase.dll", CharSet = CharSet.Unicode)]
    private static extern int WindowsCreateString(string value, uint length, out IntPtr hstring);
    [DllImport("combase.dll")]
    private static extern int WindowsDeleteString(IntPtr hstring);
    [DllImport("combase.dll")]
    private static extern int RoActivateInstance(IntPtr className, out IntPtr instance);
    [DllImport("CoreMessaging.dll", EntryPoint = "CreateDispatcherQueueController")]
    private static extern int CreateDispatcherQueueController(DispatcherQueueOptions options, out IntPtr controller);

    public static string Run(string directory)
    {
        string stage = "create STA thread";
        string error = null;
        string pixelSummary = "pixel check not run";
        IntPtr desktop = IntPtr.Zero;
        var thread = new Thread(() =>
        {
            IntPtr queue = IntPtr.Zero;
            IntPtr compositor = IntPtr.Zero;
            IntPtr uiSettings = IntPtr.Zero;
            IntPtr bootstrap = IntPtr.Zero;
            IntPtr settingsHost = IntPtr.Zero;
            IntPtr owner = IntPtr.Zero;
            bool comInitialized = false;
            bool bootstrapInitialized = false;
            SettingsShutdown shutdownSettings = null;
            BootstrapShutdown shutdownBootstrap = null;
            try
            {
                stage = "select isolated desktop";
                desktop = CreateDesktopW("OpenShazamSettingsSmoke-" + Guid.NewGuid().ToString("N"), IntPtr.Zero, IntPtr.Zero, 0, 0x02000000, IntPtr.Zero);
                if (desktop == IntPtr.Zero) throw new InvalidOperationException("CreateDesktopW: " + Marshal.GetLastWin32Error());
                if (!SetThreadDesktop(desktop)) throw new InvalidOperationException("SetThreadDesktop: " + Marshal.GetLastWin32Error());

                stage = "initialize STA";
                int hr = CoInitializeEx(IntPtr.Zero, 2);
                if (hr < 0) Marshal.ThrowExceptionForHR(hr);
                comInitialized = true;

                stage = "create DispatcherQueue";
                var options = new DispatcherQueueOptions
                {
                    Size = (uint)Marshal.SizeOf(typeof(DispatcherQueueOptions)),
                    ThreadType = 2,
                    ApartmentType = 2,
                };
                hr = CreateDispatcherQueueController(options, out queue);
                if (hr < 0) Marshal.ThrowExceptionForHR(hr);

                stage = "activate Windows Composition Compositor";
                compositor = Activate("Windows.UI.Composition.Compositor");
                stage = "activate Windows UISettings";
                uiSettings = Activate("Windows.UI.ViewManagement.UISettings");

                stage = "load Windows App SDK bootstrapper";
                bootstrap = LoadLibraryW(System.IO.Path.Combine(directory, "Microsoft.WindowsAppRuntime.Bootstrap.dll"));
                if (bootstrap == IntPtr.Zero) throw new InvalidOperationException("Load bootstrapper: " + Marshal.GetLastWin32Error());
                var initialize = Marshal.GetDelegateForFunctionPointer<BootstrapInitialize>(GetProcAddress(bootstrap, "MddBootstrapInitialize"));
                shutdownBootstrap = Marshal.GetDelegateForFunctionPointer<BootstrapShutdown>(GetProcAddress(bootstrap, "MddBootstrapShutdown"));

                stage = "initialize Windows App SDK 1.8";
                hr = initialize(0x00010008, "", 0x1F40032608CC0000UL);
                if (hr < 0) Marshal.ThrowExceptionForHR(hr);
                bootstrapInitialized = true;

                stage = "load WinUI Settings host";
                settingsHost = LoadLibraryW(System.IO.Path.Combine(directory, "OpenShazam.SettingsHost.dll"));
                if (settingsHost == IntPtr.Zero) throw new InvalidOperationException("Load Settings host: " + Marshal.GetLastWin32Error());
                var show = Marshal.GetDelegateForFunctionPointer<SettingsShow>(GetProcAddress(settingsHost, "OpenShazamSettingsShow"));
                shutdownSettings = Marshal.GetDelegateForFunctionPointer<SettingsShutdown>(GetProcAddress(settingsHost, "OpenShazamSettingsShutdown"));

                stage = "create hidden owner window";
                owner = CreateWindowExW(0, "STATIC", "OpenShazam settings smoke owner", 0, 0, 0, 100, 100, IntPtr.Zero, IntPtr.Zero, IntPtr.Zero, IntPtr.Zero);
                if (owner == IntPtr.Zero) throw new InvalidOperationException("Create owner window: " + Marshal.GetLastWin32Error());

                stage = "initialize and show XAML host";
                hr = show(owner, 0); // Start in "Use system setting" mode.
                if (hr < 0) Marshal.ThrowExceptionForHR(hr);

                stage = "pump hidden desktop for XAML callbacks";
                var deadline = DateTime.UtcNow.AddMilliseconds(500);
                while (DateTime.UtcNow < deadline)
                {
                    Message message;
                    while (PeekMessageW(out message, IntPtr.Zero, 0, 0, 1))
                    {
                        TranslateMessage(ref message);
                        DispatchMessageW(ref message);
                    }
                    Thread.Sleep(10);
                }

                stage = "verify visible WinUI island child window";
                IntPtr settingsWindow = FindWindowW("OpenShazamWinUI3Preferences", null);
                if (settingsWindow == IntPtr.Zero || !IsWindowVisible(settingsWindow))
                    throw new InvalidOperationException("Settings top-level window was not visible.");
                stage = "verify native caption follows Windows appearance";
                int appsUseLightTheme = (int)(Registry.GetValue(
                    @"HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize",
                    "AppsUseLightTheme", 1) ?? 1);
                int expectedDarkCaption = appsUseLightTheme == 0 ? 1 : 0;
                int actualDarkCaption;
                int captionResult = DwmGetWindowAttribute(settingsWindow, 20,
                    out actualDarkCaption, sizeof(int));
                if (captionResult < 0)
                    captionResult = DwmGetWindowAttribute(settingsWindow, 19,
                        out actualDarkCaption, sizeof(int));
                if (captionResult < 0 || actualDarkCaption != expectedDarkCaption)
                    throw new InvalidOperationException(
                        "Native caption theme did not follow AppsUseLightTheme. " +
                        "HRESULT=0x" + captionResult.ToString("X8") +
                        "; expectedDark=" + expectedDarkCaption +
                        "; actualDark=" + actualDarkCaption);
                IntPtr islandWindow = FindWindowExW(settingsWindow, IntPtr.Zero, IntPtr.Zero, IntPtr.Zero);
                if (islandWindow == IntPtr.Zero || !IsWindowVisible(islandWindow))
                    throw new InvalidOperationException("WinUI island child window was not visible.");
                long islandStyle = GetWindowLongPtrW(islandWindow, -16).ToInt64();
                if (GetParent(islandWindow) != settingsWindow || (islandStyle & 0x40000000L) == 0)
                    throw new InvalidOperationException(
                        "WinUI site HWND is not a WS_CHILD of the Settings host. " +
                        "parent=" + GetParent(islandWindow) + "; style=0x" + islandStyle.ToString("X"));
                Rect islandRect;
                if (!GetWindowRect(islandWindow, out islandRect) ||
                    islandRect.Right <= islandRect.Left || islandRect.Bottom <= islandRect.Top)
                    throw new InvalidOperationException("WinUI island child window had empty bounds.");

                stage = "verify Settings controls in the accessibility tree";
                AutomationElement islandRoot = AutomationElement.FromHandle(islandWindow);
                if (islandRoot == null)
                    throw new InvalidOperationException("WinUI island has no UI Automation root.");
                AutomationElementCollection descendants = islandRoot.FindAll(
                    TreeScope.Descendants, Condition.TrueCondition);
                bool startupToggleFound = false;
                bool appearanceSelectorFound = false;
                bool appearanceLabelFound = false;
                AutomationElement startupToggle = null;
                AutomationElement startupLabel = null;
                AutomationElement appearanceSelector = null;
                StringBuilder names = new StringBuilder();
                for (int i = 0; i < descendants.Count; i++)
                {
                    string name = descendants[i].Current.Name;
                    if (!String.IsNullOrWhiteSpace(name))
                    {
                        if (names.Length > 0) names.Append(" | ");
                        names.Append(name).Append(" [")
                            .Append(descendants[i].Current.ControlType.ProgrammaticName)
                            .Append("] ").Append(descendants[i].Current.BoundingRectangle);
                    }
                    if ((name == "Start on sign-in" || name == "\u81EA\u52D5\u8D77\u52D5") &&
                        descendants[i].Current.ControlType == ControlType.Button)
                    {
                        startupToggleFound = true;
                        startupToggle = descendants[i];
                    }
                    if ((name == "Start on sign-in" || name == "\u81EA\u52D5\u8D77\u52D5") &&
                        descendants[i].Current.ControlType == ControlType.Text)
                        startupLabel = descendants[i];
                    if (descendants[i].Current.ControlType == ControlType.ComboBox)
                    {
                        appearanceSelectorFound = true;
                        appearanceSelector = descendants[i];
                    }
                    if (name == "Choose a mode" || name == "\u30E2\u30FC\u30C9\u3092\u9078\u3076")
                        appearanceLabelFound = true;
                }
                if (!startupToggleFound || startupLabel == null ||
                    !appearanceSelectorFound || !appearanceLabelFound)
                    throw new InvalidOperationException(
                        "Settings should expose a startup label and right-side toggle, plus the appearance selector. " +
                        "Descendant count=" + descendants.Count + "; names=" + names);

                stage = "verify Settings labels and controls are aligned in separate columns";
                var startupLabelRect = startupLabel.Current.BoundingRectangle;
                var startupToggleRect = startupToggle.Current.BoundingRectangle;
                var appearanceSelectorRect = appearanceSelector.Current.BoundingRectangle;
                if (startupLabelRect.Right <= startupLabelRect.Left ||
                    startupLabelRect.Bottom <= startupLabelRect.Top ||
                    startupToggleRect.Right <= startupToggleRect.Left ||
                    startupToggleRect.Bottom <= startupToggleRect.Top ||
                    appearanceSelectorRect.Right <= appearanceSelectorRect.Left ||
                    appearanceSelectorRect.Bottom <= appearanceSelectorRect.Top ||
                    startupLabelRect.Left >= startupToggleRect.Left ||
                    Math.Abs(startupToggleRect.Right - appearanceSelectorRect.Right) > 24.0)
                    throw new InvalidOperationException(
                        "Settings controls should be right-aligned while labels remain left-aligned. " +
                        "Startup label=" + startupLabelRect + "; startup toggle=" + startupToggleRect +
                        "; appearance selector=" + appearanceSelectorRect + "; controls=" + names);

                stage = "verify the appearance selector options";
                var expandCollapse = (ExpandCollapsePattern)appearanceSelector.GetCurrentPattern(
                    ExpandCollapsePattern.Pattern);
                expandCollapse.Expand();
                Thread.Sleep(150);
                AutomationElementCollection expandedElements = islandRoot.FindAll(
                    TreeScope.Descendants, Condition.TrueCondition);
                bool lightOptionFound = false;
                bool darkOptionFound = false;
                bool systemOptionFound = false;
                for (int i = 0; i < expandedElements.Count; i++)
                {
                    string name = expandedElements[i].Current.Name;
                    if (name == "Light" || name == "\u30E9\u30A4\u30C8") lightOptionFound = true;
                    if (name == "Dark" || name == "\u30C0\u30FC\u30AF") darkOptionFound = true;
                    if (name == "Use system setting" || name == "\u30B7\u30B9\u30C6\u30E0\u8A2D\u5B9A\u306B\u5408\u308F\u305B\u308B")
                        systemOptionFound = true;
                }
                expandCollapse.Collapse();
                if (!lightOptionFound || !darkOptionFound || !systemOptionFound)
                    throw new InvalidOperationException(
                        "Appearance selector did not expose Light, Dark and Use system setting. " +
                        "Light=" + lightOptionFound + "; Dark=" + darkOptionFound +
                        "; System=" + systemOptionFound + "; names=" + expandedElements.Count);

                stage = "verify rendered WinUI island pixels";
                int width = islandRect.Right - islandRect.Left;
                int height = islandRect.Bottom - islandRect.Top;
                int sampledNonWhitePixels = 0;
                bool pixelsCaptured;
                using (var bitmap = new Bitmap(width, height, PixelFormat.Format32bppArgb))
                using (var graphics = Graphics.FromImage(bitmap))
                {
                    graphics.Clear(Color.White);
                    IntPtr dc = graphics.GetHdc();
                    try { pixelsCaptured = PrintWindow(islandWindow, dc, 2); }
                    finally { graphics.ReleaseHdc(dc); }
                    if (pixelsCaptured)
                    {
                        for (int y = 0; y < height; y += 2)
                            for (int x = 0; x < width; x += 2)
                            {
                                Color color = bitmap.GetPixel(x, y);
                                if (color.R < 245 || color.G < 245 || color.B < 245)
                                    sampledNonWhitePixels++;
                            }
                    }
                }
                pixelSummary = "PrintWindow=" + pixelsCaptured +
                    "; sampledNonWhitePixels=" + sampledNonWhitePixels;
                if (!pixelsCaptured || sampledNonWhitePixels < 100)
                    throw new InvalidOperationException(
                        "WinUI controls are present in UI Automation but the island rendered a blank white surface. " +
                        pixelSummary);
                stage = "shutdown XAML host";
            }
            catch (Exception exception)
            {
                error = stage + ": " + exception;
            }
            finally
            {
                try { if (shutdownSettings != null) shutdownSettings(); } catch { }
                try { if (owner != IntPtr.Zero) DestroyWindow(owner); } catch { }
                try { if (bootstrapInitialized && shutdownBootstrap != null) shutdownBootstrap(); } catch { }
                if (settingsHost != IntPtr.Zero) FreeLibrary(settingsHost);
                if (bootstrap != IntPtr.Zero) FreeLibrary(bootstrap);
                if (uiSettings != IntPtr.Zero) Marshal.Release(uiSettings);
                if (compositor != IntPtr.Zero) Marshal.Release(compositor);
                if (queue != IntPtr.Zero) Marshal.Release(queue);
                if (comInitialized) CoUninitialize();
            }
        });

        thread.SetApartmentState(ApartmentState.STA);
        thread.Start();
        thread.Join();
        if (desktop != IntPtr.Zero) CloseDesktop(desktop);
        return error ?? "Composition + WinUI Settings host initialized and shut down successfully; " + pixelSummary;
    }

    private static IntPtr Activate(string className)
    {
        IntPtr hstring;
        int hr = WindowsCreateString(className, (uint)className.Length, out hstring);
        if (hr < 0) Marshal.ThrowExceptionForHR(hr);
        try
        {
            IntPtr instance;
            hr = RoActivateInstance(hstring, out instance);
            if (hr < 0) Marshal.ThrowExceptionForHR(hr);
            return instance;
        }
        finally
        {
            WindowsDeleteString(hstring);
        }
    }
}
'@

$uiaClientAssembly = [Reflection.Assembly]::LoadWithPartialName('UIAutomationClient')
$uiaTypesAssembly = [Reflection.Assembly]::LoadWithPartialName('UIAutomationTypes')
$drawingAssembly = [Reflection.Assembly]::LoadWithPartialName('System.Drawing')
$windowsBaseAssembly = [Reflection.Assembly]::LoadWithPartialName('WindowsBase')
if (-not $uiaClientAssembly -or -not $uiaTypesAssembly -or -not $drawingAssembly -or -not $windowsBaseAssembly) {
    throw 'Windows UI Automation and System.Drawing assemblies are required for the Settings smoke test.'
}
$uiaClient = $uiaClientAssembly.Location
$uiaTypes = $uiaTypesAssembly.Location
$drawing = $drawingAssembly.Location
$windowsBase = $windowsBaseAssembly.Location
Add-Type -TypeDefinition $source -Language CSharp -ReferencedAssemblies @($uiaClient, $uiaTypes, $drawing, $windowsBase)
$result = [OpenShazamSettingsHostSmoke]::Run($dist)
if ($result -notlike 'Composition + WinUI Settings host initialized*') {
    throw $result
}
Write-Host $result
