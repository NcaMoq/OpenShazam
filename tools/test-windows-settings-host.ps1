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

    [StructLayout(LayoutKind.Sequential)]
    public struct Point
    {
        public int X;
        public int Y;
    }

    [StructLayout(LayoutKind.Sequential)]
    public struct MinMaxInfo
    {
        public Point Reserved;
        public Point MaxSize;
        public Point MaxPosition;
        public Point MinTrackSize;
        public Point MaxTrackSize;
    }

    [UnmanagedFunctionPointer(CallingConvention.StdCall)]
    private delegate int BootstrapInitialize(uint release, [MarshalAs(UnmanagedType.LPWStr)] string tag, ulong minVersion);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)]
    private delegate void BootstrapShutdown();
    [UnmanagedFunctionPointer(CallingConvention.StdCall)]
    private delegate int SettingsShow(IntPtr owner, uint appearanceMode,
        uint shortcutModifiers, uint shortcutKey, uint languageId,
        [MarshalAs(UnmanagedType.LPWStr)] string version);
    [UnmanagedFunctionPointer(CallingConvention.StdCall)]
    private delegate void SettingsShutdown();

    private const uint LoadLibrarySearchDllLoadDir = 0x00000100;
    private const uint LoadLibrarySearchSystem32 = 0x00000800;
    private const uint ProductDllSearchFlags = LoadLibrarySearchDllLoadDir | LoadLibrarySearchSystem32;

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
    [DllImport("user32.dll")]
    private static extern IntPtr GetActiveWindow();
    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool GetWindowRect(IntPtr window, out Rect rect);
    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool SetWindowPos(IntPtr window, IntPtr insertAfter,
        int x, int y, int width, int height, uint flags);
    [DllImport("user32.dll", EntryPoint = "SendMessageW", SetLastError = true)]
    private static extern IntPtr SendMessage(IntPtr window, uint message, IntPtr wParam, IntPtr lParam);
    [DllImport("user32.dll", SetLastError = true)]
    private static extern uint GetDpiForWindow(IntPtr window);
    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool PrintWindow(IntPtr window, IntPtr deviceContext, uint flags);
    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool DestroyWindow(IntPtr window);
    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool PeekMessageW(out Message message, IntPtr window, uint min, uint max, uint remove);
    [DllImport("user32.dll")]
    private static extern bool TranslateMessage(ref Message message);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern IntPtr DispatchMessageW(ref Message message);

    [DllImport("kernel32.dll", EntryPoint = "LoadLibraryExW", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern IntPtr LoadLibraryExW(string path, IntPtr file, uint flags);
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
        string minimumSizeSummary = "minimum size check not run";
        string dpiChangeSummary = "DPI change message check not run";
        string alignmentSummary = "alignment check not run";
        string appearanceDropdownSummary = "appearance dropdown check not run";
        string closeActivationSummary = "close activation check not run";
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
                bootstrap = LoadLibraryExW(System.IO.Path.Combine(directory, "Microsoft.WindowsAppRuntime.Bootstrap.dll"), IntPtr.Zero, ProductDllSearchFlags);
                if (bootstrap == IntPtr.Zero) throw new InvalidOperationException("Load bootstrapper: " + Marshal.GetLastWin32Error());
                var initialize = Marshal.GetDelegateForFunctionPointer<BootstrapInitialize>(GetProcAddress(bootstrap, "MddBootstrapInitialize"));
                shutdownBootstrap = Marshal.GetDelegateForFunctionPointer<BootstrapShutdown>(GetProcAddress(bootstrap, "MddBootstrapShutdown"));

                stage = "initialize Windows App SDK 1.8";
                hr = initialize(0x00010008, "", 0x1F40032608CC0000UL);
                if (hr < 0) Marshal.ThrowExceptionForHR(hr);
                bootstrapInitialized = true;

                stage = "load WinUI Settings host";
                settingsHost = LoadLibraryExW(System.IO.Path.Combine(directory, "OpenShazam.SettingsHost.dll"), IntPtr.Zero, ProductDllSearchFlags);
                if (settingsHost == IntPtr.Zero) throw new InvalidOperationException("Load Settings host: " + Marshal.GetLastWin32Error());
                var show = Marshal.GetDelegateForFunctionPointer<SettingsShow>(GetProcAddress(settingsHost, "OpenShazamSettingsShow"));
                shutdownSettings = Marshal.GetDelegateForFunctionPointer<SettingsShutdown>(GetProcAddress(settingsHost, "OpenShazamSettingsShutdown"));

                stage = "create visible isolated-desktop owner window";
                owner = CreateWindowExW(0, "STATIC", "OpenShazam settings smoke owner", 0x10000000, 0, 0, 100, 100, IntPtr.Zero, IntPtr.Zero, IntPtr.Zero, IntPtr.Zero);
                if (owner == IntPtr.Zero) throw new InvalidOperationException("Create owner window: " + Marshal.GetLastWin32Error());

                stage = "initialize and show XAML host";
                hr = show(owner, 2, 0x03, 0x53, 2, "0.1.1"); // Dark theme, Ctrl+Alt+S, English.
                if (hr < 0) Marshal.ThrowExceptionForHR(hr);

                stage = "pump hidden desktop for XAML callbacks";
                var deadline = DateTime.UtcNow.AddMilliseconds(1500);
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
                stage = "verify DPI-aware minimum client size";
                IntPtr minMaxBuffer = Marshal.AllocHGlobal(Marshal.SizeOf(typeof(MinMaxInfo)));
                MinMaxInfo minMax;
                try
                {
                    Marshal.StructureToPtr(new MinMaxInfo(), minMaxBuffer, false);
                    SendMessage(settingsWindow, 0x0024, IntPtr.Zero, minMaxBuffer); // WM_GETMINMAXINFO
                    minMax = Marshal.PtrToStructure<MinMaxInfo>(minMaxBuffer);
                }
                finally
                {
                    Marshal.FreeHGlobal(minMaxBuffer);
                }
                uint dpi = GetDpiForWindow(settingsWindow);
                if (dpi < 96) dpi = 96;
                int minimumClientWidth = (int)Math.Ceiling(690.0 * dpi / 96.0);
                int minimumClientHeight = (int)Math.Ceiling(320.0 * dpi / 96.0);
                if (minMax.MinTrackSize.X <= minimumClientWidth ||
                    minMax.MinTrackSize.Y <= minimumClientHeight)
                    throw new InvalidOperationException(
                        "Minimum outer tracking bounds do not preserve the required client area. " +
                        "DPI=" + dpi + "; minTrack=" + minMax.MinTrackSize.X + "x" + minMax.MinTrackSize.Y +
                        "; minimumClient=" + minimumClientWidth + "x" + minimumClientHeight);
                minimumSizeSummary = "DPI=" + dpi + "; minimumOuterTrack=" +
                    minMax.MinTrackSize.X + "x" + minMax.MinTrackSize.Y +
                    "; minimumClient=" + minimumClientWidth + "x" + minimumClientHeight;
                if (!SetWindowPos(settingsWindow, IntPtr.Zero, 0, 0,
                    minMax.MinTrackSize.X, minMax.MinTrackSize.Y, 0x0016))
                    throw new InvalidOperationException(
                        "Could not resize the hidden Settings window to its minimum track size. " +
                        Marshal.GetLastWin32Error());
                var resizeDeadline = DateTime.UtcNow.AddMilliseconds(300);
                while (DateTime.UtcNow < resizeDeadline)
                {
                    Message resizeMessage;
                    while (PeekMessageW(out resizeMessage, IntPtr.Zero, 0, 0, 1))
                    {
                        TranslateMessage(ref resizeMessage);
                        DispatchMessageW(ref resizeMessage);
                    }
                    Thread.Sleep(10);
                }
                minimumSizeSummary += "; resizedToMinimumOuterTrack=true";

                stage = "verify WM_DPICHANGED applies the proposed window bounds";
                Rect suggestedDpiBounds = new Rect
                {
                    Left = 24,
                    Top = 32,
                    Right = 24 + minMax.MinTrackSize.X + 80,
                    Bottom = 32 + minMax.MinTrackSize.Y + 60,
                };
                IntPtr dpiBoundsBuffer = Marshal.AllocHGlobal(Marshal.SizeOf(typeof(Rect)));
                Rect appliedDpiBounds;
                try
                {
                    Marshal.StructureToPtr(suggestedDpiBounds, dpiBoundsBuffer, false);
                    // Exercise the per-monitor DPI transition handler with 144 DPI.
                    SendMessage(settingsWindow, 0x02E0,
                        new IntPtr((144 << 16) | 144), dpiBoundsBuffer); // WM_DPICHANGED
                    if (!GetWindowRect(settingsWindow, out appliedDpiBounds))
                        throw new InvalidOperationException(
                            "Could not read the Settings bounds after WM_DPICHANGED. " +
                            Marshal.GetLastWin32Error());
                }
                finally
                {
                    Marshal.FreeHGlobal(dpiBoundsBuffer);
                }
                if (appliedDpiBounds.Left != suggestedDpiBounds.Left ||
                    appliedDpiBounds.Top != suggestedDpiBounds.Top ||
                    appliedDpiBounds.Right != suggestedDpiBounds.Right ||
                    appliedDpiBounds.Bottom != suggestedDpiBounds.Bottom)
                    throw new InvalidOperationException(
                        "The Settings host did not apply the WM_DPICHANGED suggested bounds. " +
                        "expected=" + suggestedDpiBounds.Left + "," + suggestedDpiBounds.Top + "," +
                            suggestedDpiBounds.Right + "," + suggestedDpiBounds.Bottom +
                        "; actual=" + appliedDpiBounds.Left + "," + appliedDpiBounds.Top + "," +
                            appliedDpiBounds.Right + "," + appliedDpiBounds.Bottom);
                Rect beforeInvalidDpiBounds;
                if (!GetWindowRect(settingsWindow, out beforeInvalidDpiBounds))
                    throw new InvalidOperationException(
                        "Could not read the Settings bounds before testing an invalid DPI rectangle. " +
                        Marshal.GetLastWin32Error());
                Rect invalidDpiBounds = new Rect
                {
                    Left = 0,
                    Top = 0,
                    Right = 32769,
                    Bottom = 400,
                };
                IntPtr invalidDpiBoundsBuffer = Marshal.AllocHGlobal(Marshal.SizeOf(typeof(Rect)));
                Rect afterInvalidDpiBounds;
                try
                {
                    Marshal.StructureToPtr(invalidDpiBounds, invalidDpiBoundsBuffer, false);
                    SendMessage(settingsWindow, 0x02E0,
                        new IntPtr((144 << 16) | 144), invalidDpiBoundsBuffer); // WM_DPICHANGED
                    if (!GetWindowRect(settingsWindow, out afterInvalidDpiBounds))
                        throw new InvalidOperationException(
                            "Could not read the Settings bounds after rejecting an invalid DPI rectangle. " +
                            Marshal.GetLastWin32Error());
                }
                finally
                {
                    Marshal.FreeHGlobal(invalidDpiBoundsBuffer);
                }
                if (afterInvalidDpiBounds.Left != beforeInvalidDpiBounds.Left ||
                    afterInvalidDpiBounds.Top != beforeInvalidDpiBounds.Top ||
                    afterInvalidDpiBounds.Right != beforeInvalidDpiBounds.Right ||
                    afterInvalidDpiBounds.Bottom != beforeInvalidDpiBounds.Bottom)
                    throw new InvalidOperationException(
                        "The Settings host applied an invalid oversized DPI rectangle.");
                dpiChangeSummary =
                    "simulatedDpi=144; suggestedBoundsApplied=true; oversizedBoundsIgnored=true";

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
                AutomationElement hostRoot = AutomationElement.FromHandle(settingsWindow);
                AutomationElementCollection descendants = hostRoot.FindAll(
                    TreeScope.Descendants, Condition.TrueCondition);
                bool startupToggleFound = false;
                bool startupReadFailureFound = false;
                bool startupReadFailureMessageFound = false;
                bool appearanceSelectorFound = false;
                bool languageSelectorFound = false;
                bool appearanceLabelFound = false;
                bool updateActionFound = false;
                bool automaticUpdateToggleFound = false;
                AutomationElement startupDescription = null;
                AutomationElement appearanceDescription = null;
                AutomationElement languageDescription = null;
                AutomationElement startupToggle = null;
                AutomationElement startupLabel = null;
                AutomationElement appearanceSelector = null;
                AutomationElement languageSelector = null;
                AutomationElement automaticUpdateToggle = null;
                AutomationElement updateActionButton = null;
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
                    if (name == "Could not read start on sign-in")
                        startupReadFailureFound = true;
                    if (name == "Windows did not return the current state. Reopen Settings and try again.")
                        startupReadFailureMessageFound = true;
                    if ((name == "Open at startup" || name == "\u8D77\u52D5\u6642\u306B\u958B\u304F") &&
                        descendants[i].Current.ControlType == ControlType.Button)
                    {
                        startupToggleFound = true;
                        startupToggle = descendants[i];
                    }
                    if (!startupToggleFound &&
                        (name == "Open at startup" || name == "\u8D77\u52D5\u6642\u306B\u958B\u304F"))
                    {
                        try
                        {
                            if (descendants[i].GetCurrentPattern(TogglePattern.Pattern) is TogglePattern)
                            {
                                startupToggleFound = true;
                                startupToggle = descendants[i];
                            }
                        }
                        catch (InvalidOperationException) { }
                    }
                    if ((name == "Open at startup" || name == "\u8D77\u52D5\u6642\u306B\u958B\u304F") &&
                        descendants[i].Current.ControlType == ControlType.Text)
                        startupLabel = descendants[i];
                    if ((name == "Start Shazam every time Windows starts" ||
                        name == "Windows\u3092\u8D77\u52D5\u3059\u308B\u305F\u3073\u306BShazam\u30A2\u30D7\u30EA\u3082\u8D77\u52D5") &&
                        descendants[i].Current.ControlType == ControlType.Text)
                        startupDescription = descendants[i];
                    if (descendants[i].Current.ControlType == ControlType.ComboBox)
                    {
                        if (name == "Theme" || name == "\u30C6\u30FC\u30DE")
                        {
                            appearanceSelectorFound = true;
                            appearanceSelector = descendants[i];
                        }
                        if (name == "Language" || name == "\u8A00\u8A9E")
                        {
                            languageSelectorFound = true;
                            languageSelector = descendants[i];
                        }
                    }
                    if (name == "Automatic updates" || name == "\u81EA\u52D5\u66F4\u65B0")
                    {
                        try
                        {
                            if (descendants[i].GetCurrentPattern(TogglePattern.Pattern) is TogglePattern)
                            {
                                automaticUpdateToggleFound = true;
                                automaticUpdateToggle = descendants[i];
                            }
                        }
                        catch (InvalidOperationException) { }
                    }
                    if (name == "Check for updates" &&
                        descendants[i].Current.ControlType == ControlType.Button)
                    {
                        updateActionFound = true;
                        updateActionButton = descendants[i];
                    }
                    if (name == "Theme" || name == "\u30C6\u30FC\u30DE")
                        appearanceLabelFound = true;
                    if ((name == "Choose the app theme to display" ||
                        name == "\u8868\u793A\u3059\u308B\u30A2\u30D7\u30EA\u306E\u30C6\u30FC\u30DE\u3092\u9078\u629E\u3057\u307E\u3059") &&
                        descendants[i].Current.ControlType == ControlType.Text)
                        appearanceDescription = descendants[i];
                    if ((name == "Choose the display language." ||
                        name == "\u8868\u793A\u8A00\u8A9E\u3092\u9078\u629E\u3057\u307E\u3059") &&
                        descendants[i].Current.ControlType == ControlType.Text)
                        languageDescription = descendants[i];
                }
                if ((!startupToggleFound &&
                        (!startupReadFailureFound || !startupReadFailureMessageFound)) ||
                    startupLabel == null || startupDescription == null ||
                    appearanceDescription == null || languageDescription == null ||
                    !appearanceSelectorFound || !languageSelectorFound ||
                    !appearanceLabelFound || !updateActionFound || !automaticUpdateToggleFound)
                    throw new InvalidOperationException(
                        "Settings should expose wrapped startup/theme/language descriptions, both named selectors, the automatic-update toggle, a startup label and either a right-side toggle or the explicit startup-read error, plus the update check action. " +
                        "Descendant count=" + descendants.Count + "; names=" + names);

                stage = "verify Settings labels and controls are aligned in separate columns";
                var startupLabelRect = startupLabel.Current.BoundingRectangle;
                var startupDescriptionRect = startupDescription.Current.BoundingRectangle;
                var appearanceDescriptionRect = appearanceDescription.Current.BoundingRectangle;
                var appearanceSelectorRect = appearanceSelector.Current.BoundingRectangle;
                if (startupLabelRect.Right <= startupLabelRect.Left ||
                    startupLabelRect.Bottom <= startupLabelRect.Top ||
                    startupDescriptionRect.Right <= startupDescriptionRect.Left ||
                    startupDescriptionRect.Bottom <= startupDescriptionRect.Top ||
                    appearanceDescriptionRect.Right <= appearanceDescriptionRect.Left ||
                    appearanceDescriptionRect.Bottom <= appearanceDescriptionRect.Top ||
                    appearanceSelectorRect.Right <= appearanceSelectorRect.Left ||
                    appearanceSelectorRect.Bottom <= appearanceSelectorRect.Top ||
                    (startupToggleFound &&
                        (startupToggle.Current.BoundingRectangle.Right <= startupToggle.Current.BoundingRectangle.Left ||
                         startupToggle.Current.BoundingRectangle.Bottom <= startupToggle.Current.BoundingRectangle.Top ||
                         startupLabelRect.Left >= startupToggle.Current.BoundingRectangle.Left ||
                         Math.Abs(startupToggle.Current.BoundingRectangle.Right - appearanceSelectorRect.Right) > 24.0)))
                    throw new InvalidOperationException(
                        "Settings controls should be right-aligned while labels remain left-aligned. " +
                        "Startup label=" + startupLabelRect + "; appearance selector=" + appearanceSelectorRect +
                        "; controls=" + names);
                if (startupToggleFound)
                {
                    var startupToggleRect = startupToggle.Current.BoundingRectangle;
                    alignmentSummary = "startupControlRight=" + startupToggleRect.Right +
                        "; appearanceControlRight=" + appearanceSelectorRect.Right +
                        "; difference=" + Math.Abs(startupToggleRect.Right - appearanceSelectorRect.Right);
                }
                else
                {
                    alignmentSummary = "startupReadFailureShown=true; appearanceControlRight=" +
                        appearanceSelectorRect.Right;
                }

                stage = "verify the appearance selector options";
                var expandCollapse = (ExpandCollapsePattern)appearanceSelector.GetCurrentPattern(
                    ExpandCollapsePattern.Pattern);
                expandCollapse.Expand();
                var dropdownDeadline = DateTime.UtcNow.AddMilliseconds(400);
                while (DateTime.UtcNow < dropdownDeadline)
                {
                    Message dropdownMessage;
                    while (PeekMessageW(out dropdownMessage, IntPtr.Zero, 0, 0, 1))
                    {
                        TranslateMessage(ref dropdownMessage);
                        DispatchMessageW(ref dropdownMessage);
                    }
                    Thread.Sleep(10);
                }
                if (expandCollapse.Current.ExpandCollapseState != ExpandCollapseState.Expanded)
                    throw new InvalidOperationException(
                        "Appearance selector did not enter the expanded UI Automation state.");
                expandCollapse.Collapse();
                var themeSelection = ((SelectionPattern)appearanceSelector.GetCurrentPattern(
                    SelectionPattern.Pattern)).Current.GetSelection();
                if (themeSelection.Length != 1 || themeSelection[0].Current.Name != "Dark")
                    throw new InvalidOperationException(
                        "The explicitly named Theme selector did not retain the requested Dark value.");
                appearanceDropdownSummary = "themeSelector=" + appearanceSelectorRect.Width +
                    "px; expandsAndCollapses=true; selected=Dark";

                stage = "verify the separately named language selector";
                var languageExpandCollapse = (ExpandCollapsePattern)languageSelector.GetCurrentPattern(
                    ExpandCollapsePattern.Pattern);
                languageExpandCollapse.Expand();
                var languageDropdownDeadline = DateTime.UtcNow.AddMilliseconds(400);
                while (DateTime.UtcNow < languageDropdownDeadline)
                {
                    Message languageMessage;
                    while (PeekMessageW(out languageMessage, IntPtr.Zero, 0, 0, 1))
                    {
                        TranslateMessage(ref languageMessage);
                        DispatchMessageW(ref languageMessage);
                    }
                    Thread.Sleep(10);
                }
                if (languageExpandCollapse.Current.ExpandCollapseState != ExpandCollapseState.Expanded)
                    throw new InvalidOperationException(
                        "Language selector did not enter the expanded UI Automation state.");
                languageExpandCollapse.Collapse();
                var languageSelection = ((SelectionPattern)languageSelector.GetCurrentPattern(
                    SelectionPattern.Pattern)).Current.GetSelection();
                if (languageSelection.Length != 1 || languageSelection[0].Current.Name != "English")
                    throw new InvalidOperationException(
                        "The separately named Language selector did not retain the requested English value.");
                appearanceDropdownSummary += "; languageSelector=" +
                    languageSelector.Current.BoundingRectangle.Width +
                    "px; expandsAndCollapses=true; selected=English";

                stage = "verify automatic updates default to enabled";
                var automaticUpdatePattern = (TogglePattern)automaticUpdateToggle.GetCurrentPattern(
                    TogglePattern.Pattern);
                if (automaticUpdatePattern.Current.ToggleState != ToggleState.On)
                    throw new InvalidOperationException(
                        "Automatic updates should default to On in the Settings host.");
                appearanceDropdownSummary += "; autoUpdate=On";

                stage = "refresh the update check action after selector interactions";
                updateActionButton = hostRoot.FindFirst(
                    TreeScope.Descendants,
                    new AndCondition(
                        new PropertyCondition(AutomationElement.NameProperty, "Check for updates"),
                        new PropertyCondition(AutomationElement.ControlTypeProperty, ControlType.Button)));
                if (updateActionButton == null)
                    throw new InvalidOperationException(
                        "The update check action disappeared from the Settings accessibility tree.");
                stage = "scroll the update check action into view";
                var updateActionRect = updateActionButton.Current.BoundingRectangle;
                int scrollPatternsFound = 0;
                for (int attempt = 0; attempt < 8 &&
                    (updateActionButton.Current.IsOffscreen ||
                     updateActionRect.Right <= updateActionRect.Left ||
                     updateActionRect.Bottom <= updateActionRect.Top); attempt++)
                {
                    var scrollableElements = hostRoot.FindAll(
                        TreeScope.Descendants, Condition.TrueCondition);
                    for (int i = 0; i < scrollableElements.Count; i++)
                    {
                        try
                        {
                            var scroll = scrollableElements[i].GetCurrentPattern(
                                ScrollPattern.Pattern) as ScrollPattern;
                            if (scroll != null && scroll.Current.VerticallyScrollable)
                            {
                                scrollPatternsFound++;
                                scroll.Scroll(ScrollAmount.NoAmount, ScrollAmount.LargeIncrement);
                            }
                        }
                        catch (InvalidOperationException) { }
                        catch (ElementNotAvailableException) { }
                    }
                    var scrollPumpDeadline = DateTime.UtcNow.AddMilliseconds(100);
                    while (DateTime.UtcNow < scrollPumpDeadline)
                    {
                        Message updateScrollMessage;
                        while (PeekMessageW(out updateScrollMessage, IntPtr.Zero, 0, 0, 1))
                        {
                            TranslateMessage(ref updateScrollMessage);
                            DispatchMessageW(ref updateScrollMessage);
                        }
                        Thread.Sleep(10);
                    }
                    updateActionRect = updateActionButton.Current.BoundingRectangle;
                }
                if (updateActionRect.Right <= updateActionRect.Left ||
                    updateActionRect.Bottom <= updateActionRect.Top ||
                    updateActionButton.Current.IsOffscreen ||
                    Math.Abs(updateActionRect.Right - appearanceSelectorRect.Right) > 24.0)
                    throw new InvalidOperationException(
                        "The update check action should be visible and right-aligned with the theme selector. " +
                        "Update action=" + updateActionRect + "; theme selector=" + appearanceSelectorRect +
                        "; verticalScrollPatterns=" + scrollPatternsFound);
                alignmentSummary += "; updateControlRight=" + updateActionRect.Right;

                stage = "verify rendered WinUI island pixels";
                int width = islandRect.Right - islandRect.Left;
                int height = islandRect.Bottom - islandRect.Top;
                int sampledNonWhitePixels = 0;
                int sampledDarkPixels = 0;
                int sampledPixels = 0;
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
                                sampledPixels++;
                                if (color.R < 245 || color.G < 245 || color.B < 245)
                                    sampledNonWhitePixels++;
                                if (color.R < 128 && color.G < 128 && color.B < 128)
                                    sampledDarkPixels++;
                            }
                    }
                }
                pixelSummary = "PrintWindow=" + pixelsCaptured +
                    "; sampledNonWhitePixels=" + sampledNonWhitePixels +
                    "; darkSurfaceRatio=" + (sampledPixels == 0 ? 0.0 :
                        (double)sampledDarkPixels / sampledPixels).ToString("P1");
                if (!pixelsCaptured || sampledNonWhitePixels < 100)
                    throw new InvalidOperationException(
                        "WinUI controls are present in UI Automation but the island rendered a blank white surface. " +
                        pixelSummary);
                if (sampledDarkPixels < sampledPixels / 2)
                    throw new InvalidOperationException(
                        "The explicitly selected dark theme did not render a predominantly dark WinUI surface. " +
                        pixelSummary);
                stage = "verify closing Settings restores owner activation";
                SendMessage(settingsWindow, 0x0010, IntPtr.Zero, IntPtr.Zero); // WM_CLOSE
                if (IsWindowVisible(settingsWindow))
                    throw new InvalidOperationException("Settings remained visible after WM_CLOSE.");
                IntPtr activeWindow = GetActiveWindow();
                if (activeWindow != owner)
                    throw new InvalidOperationException(
                        "Closing Settings should restore activation to its visible owner. " +
                        "owner=" + owner + "; activeWindow=" + activeWindow);
                closeActivationSummary = "closeRestoredOwnerActivation=true";
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

        thread.IsBackground = true;
        thread.SetApartmentState(ApartmentState.STA);
        thread.Start();
        if (!thread.Join(TimeSpan.FromSeconds(60)))
        {
            // UI Automation can block inside a provider call. Do not let this
            // smoke test hang CI forever; the isolated desktop and native
            // resources are reclaimed when the test process exits.
            return "Timed out after 60 seconds while running the Settings UI smoke test; " +
                "last observed stage=" + stage + ".";
        }
        if (desktop != IntPtr.Zero) CloseDesktop(desktop);
        return error ?? "Composition + WinUI Settings host initialized and shut down successfully; " +
            minimumSizeSummary + "; " + dpiChangeSummary + "; " + alignmentSummary + "; " + appearanceDropdownSummary +
            "; " + pixelSummary + "; " + closeActivationSummary;
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
# PowerShell 6+ does not add framework assemblies automatically when an explicit
# -ReferencedAssemblies list is provided, so include the assembly defining Thread.
$threadingAssembly = [System.Threading.Thread].Assembly.Location
if (-not $uiaClientAssembly -or -not $uiaTypesAssembly -or -not $drawingAssembly -or -not $windowsBaseAssembly -or -not $threadingAssembly) {
    throw 'Windows UI Automation, System.Drawing, and threading assemblies are required for the Settings smoke test.'
}
$uiaClient = $uiaClientAssembly.Location
$uiaTypes = $uiaTypesAssembly.Location
$drawing = $drawingAssembly.Location
$windowsBase = $windowsBaseAssembly.Location
Add-Type -TypeDefinition $source -Language CSharp -ReferencedAssemblies @($uiaClient, $uiaTypes, $drawing, $windowsBase, $threadingAssembly)
$result = [OpenShazamSettingsHostSmoke]::Run($dist)
if ($result -notlike 'Composition + WinUI Settings host initialized*') {
    throw $result
}
Write-Host $result
