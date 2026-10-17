// WinUI 3 Settings XAML Island hosted IN the native Rust/Win32 process.
// No second recognition controller, process, webview, or legacy Win32 settings UI.
#include "pch.h"
#include <appmodel.h>
#include <winrt/Windows.Foundation.Collections.h>
#include <winrt/Microsoft.UI.Xaml.Automation.h>

namespace x = winrt::Microsoft::UI::Xaml;
namespace controls = winrt::Microsoft::UI::Xaml::Controls;
namespace hosting = winrt::Microsoft::UI::Xaml::Hosting;
namespace dispatch = winrt::Microsoft::UI::Dispatching;
namespace automation = winrt::Microsoft::UI::Xaml::Automation;
namespace markup = winrt::Microsoft::UI::Xaml::Markup;
namespace typeinfo = winrt::Microsoft::UI::Xaml::XamlTypeInfo;
namespace appmodel = winrt::Windows::ApplicationModel;

namespace
{
    // WinUI XAML Islands require an active Application for control metadata
    // and XamlControlsResources, even when content is authored in C++/WinRT.
    // Keep the framework initialized on the same STA/DispatcherQueue thread.
    struct SettingsApplication : x::ApplicationT<SettingsApplication, markup::IXamlMetadataProvider>
    {
        SettingsApplication()
            : xamlManager(hosting::WindowsXamlManager::InitializeForCurrentThread())
        {
        }

        markup::IXamlType GetXamlType(
            winrt::Windows::UI::Xaml::Interop::TypeName const& name)
        {
            return provider.GetXamlType(name);
        }

        markup::IXamlType GetXamlType(winrt::hstring const& name)
        {
            return provider.GetXamlType(name);
        }

        winrt::com_array<markup::XmlnsDefinition> GetXmlnsDefinitions()
        {
            return provider.GetXmlnsDefinitions();
        }

    private:
        hosting::WindowsXamlManager xamlManager{ nullptr };
        typeinfo::XamlControlsXamlMetaDataProvider provider;
    };

    constexpr wchar_t kSettingsClass[] = L"OpenShazamWinUI3Preferences";
    constexpr wchar_t kStartupTask[] = L"OpenShazamStartup";
    constexpr int kWidthDips = 520;
    constexpr int kHeightDips = 380;

    bool IsJapanese() noexcept
    {
        return PRIMARYLANGID(::GetUserDefaultUILanguage()) == LANG_JAPANESE;
    }

    winrt::hstring Localized(wchar_t const* en, wchar_t const* ja)
    {
        return IsJapanese() ? winrt::hstring(ja) : winrt::hstring(en);
    }

    struct SettingsState
    {
        controls::ToggleSwitch toggle{ nullptr };
        controls::InfoBar info{ nullptr };
        bool loading{ true };
        bool alive{ true };

        void Status(winrt::hstring const& message, controls::InfoBarSeverity level)
        {
            if (!alive) return;
            info.Title(Localized(L"Startup setting", L"自動起動"));
            info.Severity(level);
            info.Message(message);
            info.IsOpen(true);
        }
    };

    bool IsPackagedApp() noexcept
    {
        UINT32 packageNameLength = 0;
        const auto result = ::GetCurrentPackageFullName(&packageNameLength, nullptr);
        return result != APPMODEL_ERROR_NO_PACKAGE;
    }

    void ShowStartupFailureNoThrow(
        std::shared_ptr<SettingsState> const& state,
        winrt::hstring const& message = {},
        controls::InfoBarSeverity severity = controls::InfoBarSeverity::Error,
        bool unexpectedFailure = false) noexcept
    {
        if (!state || !state->alive) return;
        state->loading = false;
        try
        {
            state->toggle.IsOn(false);
            state->toggle.IsEnabled(false);
            if (!message.empty())
            {
                state->Status(message, severity);
            }
            else if (!unexpectedFailure && !IsPackagedApp())
            {
                state->Status(Localized(
                    L"Automatic startup is available in the packaged MSIX version.",
                    L"自動起動はMSIXパッケージ版で利用できます。"),
                    controls::InfoBarSeverity::Informational);
            }
            else
            {
                state->Status(Localized(
                    L"Windows could not read or update the startup setting.",
                    L"Windowsで自動起動の設定を読み込めませんでした。"),
                    controls::InfoBarSeverity::Error);
            }
        }
        catch (...)
        {
            // WinRT property updates can fail while the island is closing.
            // Never allow an exception to escape a fire_and_forget coroutine.
        }
    }

    winrt::fire_and_forget RefreshStartup(
        std::weak_ptr<SettingsState> weak,
        winrt::hstring failureMessage = {},
        controls::InfoBarSeverity failureSeverity = controls::InfoBarSeverity::Error)
    {
        auto state = weak.lock();
        if (!state || !state->alive) co_return;
        try
        {
            state->loading = true;
            state->toggle.IsEnabled(false);
            auto task = co_await appmodel::StartupTask::GetAsync(kStartupTask);
            if (!state->alive) co_return;
            auto current = task.State();
            state->toggle.IsOn(current == appmodel::StartupTaskState::Enabled ||
                               current == appmodel::StartupTaskState::EnabledByPolicy);
            state->toggle.IsEnabled(current == appmodel::StartupTaskState::Disabled ||
                                    current == appmodel::StartupTaskState::Enabled);
            switch (current)
            {
            case appmodel::StartupTaskState::DisabledByUser:
                state->Status(Localized(L"Startup was disabled in Windows. Re-enable it in Windows Settings.",
                    L"Windows側で自動起動が無効です。Windowsの設定で有効にしてください。"),
                    controls::InfoBarSeverity::Informational);
                break;
            case appmodel::StartupTaskState::DisabledByPolicy:
                state->Status(Localized(L"Startup is restricted by organization policy.",
                    L"組織のポリシーにより自動起動が制限されています。"),
                    controls::InfoBarSeverity::Informational);
                break;
            case appmodel::StartupTaskState::EnabledByPolicy:
                state->Status(Localized(L"Startup is enabled by organization policy.",
                    L"組織のポリシーにより自動起動が有効です。"),
                    controls::InfoBarSeverity::Informational);
                break;
            default:
                state->info.IsOpen(false);
                break;
            }
            if (!failureMessage.empty())
                state->Status(failureMessage, failureSeverity);
            state->loading = false;
        }
        catch (winrt::hresult_error const&)
        {
            ShowStartupFailureNoThrow(state, failureMessage, failureSeverity);
        }
        catch (...)
        {
            ShowStartupFailureNoThrow(state, failureMessage, failureSeverity, true);
        }
    }

    winrt::fire_and_forget ChangeStartup(std::weak_ptr<SettingsState> weak, bool enable)
    {
        auto state = weak.lock();
        if (!state || !state->alive) co_return;
        try
        {
            state->loading = true;
            state->toggle.IsEnabled(false);
            auto task = co_await appmodel::StartupTask::GetAsync(kStartupTask);
            if (!state->alive) co_return;
            if (enable)
            {
                co_await task.RequestEnableAsync();
            }
            else
            {
                task.Disable();
            }
            if (!state->alive) co_return;
            RefreshStartup(weak);
        }
        catch (winrt::hresult_error const&)
        {
            try
            {
                ShowStartupFailureNoThrow(state, Localized(
                    L"Windows could not update the startup setting.",
                    L"Windowsで自動起動の設定を変更できませんでした。"),
                    controls::InfoBarSeverity::Error);
            }
            catch (...)
            {
                ShowStartupFailureNoThrow(state);
            }
        }
        catch (...)
        {
            ShowStartupFailureNoThrow(state, {}, controls::InfoBarSeverity::Error, true);
        }
    }

    struct SettingsHost;
    SettingsHost* g_active{};

    struct SettingsHost
    {
        HWND hwnd{};
        HWND owner{};
        HWND islandHwnd{};
        hosting::DesktopWindowXamlSource island{ nullptr };
        dispatch::DispatcherQueueController queue{ nullptr };
        x::Application app{ nullptr };
        bool ownsQueue{};
        std::shared_ptr<SettingsState> state;

        void Initialize(HWND parent)
        {
            owner = parent;
            // The native application has an STA COM apartment and a USER32
            // message pump. This is only created on the first Settings request.
            // Reuse a Microsoft.UI queue if another in-process host initialized it.
            if (!dispatch::DispatcherQueue::GetForCurrentThread())
            {
                queue = dispatch::DispatcherQueueController::CreateOnCurrentThread();
                ownsQueue = true;
            }
            // Instantiate the XAML Application after the queue and before any
            // island/control. It supplies WinUI metadata and owns the manager.
            app = winrt::make<SettingsApplication>();
            app.Resources().MergedDictionaries().Append(controls::XamlControlsResources{});
            auto instance = ::GetModuleHandleW(nullptr);
            WNDCLASSW wc{};
            wc.lpfnWndProc = &WindowProc;
            wc.hInstance = instance;
            wc.lpszClassName = kSettingsClass;
            wc.hCursor = ::LoadCursorW(nullptr, IDC_ARROW);
            wc.hbrBackground = reinterpret_cast<HBRUSH>(COLOR_WINDOW + 1);
            if (!::RegisterClassW(&wc) && ::GetLastError() != ERROR_CLASS_ALREADY_EXISTS)
                winrt::throw_last_error();

            auto dpi = ::GetDpiForWindow(parent);
            if (dpi < 96) dpi = 96;
            auto windowTitle = Localized(L"OpenShazam Settings", L"OpenShazam の設定");
            hwnd = ::CreateWindowExW(0, kSettingsClass, windowTitle.c_str(),
                WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN, CW_USEDEFAULT, CW_USEDEFAULT,
                ::MulDiv(kWidthDips, dpi, 96), ::MulDiv(kHeightDips, dpi, 96),
                parent, nullptr, instance, nullptr);
            if (!hwnd) winrt::throw_last_error();
            island = hosting::DesktopWindowXamlSource{};
            island.Initialize(winrt::Microsoft::UI::GetWindowIdFromWindow(hwnd));
            islandHwnd = winrt::Microsoft::UI::GetWindowFromWindowId(island.SiteBridge().WindowId());
            if (!islandHwnd || !::IsWindow(islandHwnd))
                winrt::throw_hresult(E_HANDLE);
            // The WinAppSDK Islands sample sets the site HWND to an explicit
            // child window. DesktopWindowXamlSource creates the HWND, but its
            // initial style is not a reliable substitute for WS_CHILD here.
            // Without WS_CHILD the site can exist in UI Automation and still
            // fail to compose into the host window.
            ::SetLastError(ERROR_SUCCESS);
            auto previousStyle = ::SetWindowLongPtrW(
                islandHwnd, GWL_STYLE, WS_TABSTOP | WS_CHILD | WS_VISIBLE);
            auto styleError = ::GetLastError();
            if (previousStyle == 0 && styleError != ERROR_SUCCESS)
                winrt::throw_last_error();
            state = std::make_shared<SettingsState>();
            island.Content(BuildContent());
            Layout();
            RefreshStartup(state);
        }

        x::UIElement BuildContent()
        {
            controls::StackPanel stack;
            stack.Spacing(20);
            stack.Padding(x::ThicknessHelper::FromUniformLength(24));

            controls::TextBlock title;
            title.Text(Localized(L"Settings", L"設定"));
            title.FontSize(28);
            title.FontWeight(winrt::Windows::UI::Text::FontWeights::SemiBold());
            stack.Children().Append(title);

            controls::TextBlock description;
            description.Text(Localized(L"Choose how OpenShazam starts.",
                L"OpenShazamの起動方法を設定します。"));
            description.TextWrapping(x::TextWrapping::Wrap);
            stack.Children().Append(description);

            controls::TextBlock generalHeader;
            generalHeader.Text(Localized(L"General", L"全般"));
            generalHeader.FontSize(20);
            generalHeader.FontWeight(winrt::Windows::UI::Text::FontWeights::SemiBold());
            stack.Children().Append(generalHeader);

            state->toggle = controls::ToggleSwitch{};
            controls::StackPanel startupHeader;
            startupHeader.Spacing(4);
            auto startupName = Localized(
                L"Launch OpenShazam when I sign in",
                L"Windowsへのサインイン時にOpenShazamを起動");
            auto startupHelp = Localized(
                L"Start in the notification area. Music recognition will not start automatically.",
                L"サインイン時に通知領域へ起動します。楽曲認識は自動で開始しません。");
            controls::TextBlock startupTitle;
            startupTitle.Text(startupName);
            startupTitle.TextWrapping(x::TextWrapping::Wrap);
            startupTitle.FontSize(16);
            startupTitle.FontWeight(winrt::Windows::UI::Text::FontWeights::SemiBold());
            startupHeader.Children().Append(startupTitle);

            controls::TextBlock startupDescription;
            startupDescription.Text(startupHelp);
            startupDescription.TextWrapping(x::TextWrapping::Wrap);
            startupHeader.Children().Append(startupDescription);
            state->toggle.Header(startupHeader);
            automation::AutomationProperties::SetName(state->toggle, startupName);
            automation::AutomationProperties::SetHelpText(state->toggle, startupHelp);
            state->toggle.OnContent(winrt::box_value(Localized(L"On", L"オン")));
            state->toggle.OffContent(winrt::box_value(Localized(L"Off", L"オフ")));
            state->toggle.IsEnabled(false);
            std::weak_ptr<SettingsState> weak = state;
            state->toggle.Toggled([weak](auto const&, auto const&) {
                if (auto state = weak.lock(); state && state->alive && !state->loading)
                    ChangeStartup(weak, state->toggle.IsOn());
            });
            stack.Children().Append(state->toggle);

            controls::TextBlock inputHeader;
            inputHeader.Text(Localized(L"Audio input", L"音声入力"));
            inputHeader.FontSize(20);
            inputHeader.FontWeight(winrt::Windows::UI::Text::FontWeights::SemiBold());
            stack.Children().Append(inputHeader);

            controls::TextBlock inputDescription;
            inputDescription.Text(Localized(
                L"Recognition uses system audio that is currently playing on this PC.",
                L"このPCで現在再生中のシステム音声を認識します。"));
            inputDescription.TextWrapping(x::TextWrapping::Wrap);
            stack.Children().Append(inputDescription);

            state->info = controls::InfoBar{};
            state->info.IsOpen(false);
            state->info.IsClosable(false);
            stack.Children().Append(state->info);

            // Keep the background as a XAML ThemeResource expression rather
            // than copying the current brush instance. That lets the root
            // switch palettes when Windows changes app appearance at runtime.
            controls::ScrollViewer viewer = markup::XamlReader::Load(
                LR"(<ScrollViewer xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation" Background="{ThemeResource ApplicationPageBackgroundThemeBrush}" />)"
            ).as<controls::ScrollViewer>();
            viewer.Content(stack);
            viewer.VerticalScrollBarVisibility(controls::ScrollBarVisibility::Auto);
            return viewer;
        }

        void Layout()
        {
            if (!hwnd || !island) return;
            RECT rect{};
            ::GetClientRect(hwnd, &rect);
            island.SiteBridge().MoveAndResize({0, 0, rect.right - rect.left, rect.bottom - rect.top});
        }

        void Show()
        {
            if (!hwnd) return;
            ::ShowWindow(hwnd, SW_SHOWNORMAL);
            // The island is initially measured while its host HWND is hidden.
            // Reapply its native site bounds after the parent becomes visible.
            Layout();
            if (islandHwnd)
            {
                ::ShowWindow(islandHwnd, SW_SHOWNA);
                RECT rect{};
                ::GetClientRect(hwnd, &rect);
                ::SetWindowPos(islandHwnd, HWND_TOP, 0, 0,
                    rect.right - rect.left, rect.bottom - rect.top,
                    SWP_NOACTIVATE | SWP_FRAMECHANGED | SWP_SHOWWINDOW);
            }
            ::SetForegroundWindow(hwnd);
            if (islandHwnd)
            {
                ::UpdateWindow(islandHwnd);
                if (::IsWindowVisible(islandHwnd)) ::SetFocus(islandHwnd);
            }
            if (state) RefreshStartup(state);
        }

        void Close() noexcept
        {
            try
            {
                if (state) state->alive = false;
                if (island) island.Close();
                island = nullptr;
                islandHwnd = nullptr;
                if (hwnd) { auto w = hwnd; hwnd = nullptr; ::DestroyWindow(w); }
                state.reset();
                app = nullptr;
                if (queue && ownsQueue)
                {
                    queue.ShutdownQueue();
                    queue = nullptr;
                }
            }
            catch (...) { }
        }

        static LRESULT CALLBACK WindowProc(HWND h, UINT msg, WPARAM wp, LPARAM lp)
        {
            auto* host = g_active;
            switch (msg)
            {
            case WM_SIZE:
                if (host && host->hwnd == h) host->Layout();
                return 0;
            case WM_DPICHANGED:
                if (lp)
                {
                    auto rect = reinterpret_cast<RECT const*>(lp);
                    ::SetWindowPos(h, nullptr, rect->left, rect->top,
                        rect->right - rect->left, rect->bottom - rect->top,
                        SWP_NOZORDER | SWP_NOACTIVATE);
                }
                return 0;
            case WM_CLOSE:
                ::ShowWindow(h, SW_HIDE);
                return 0;
            case WM_DESTROY:
                return 0; // Never quit the Rust tray process here.
            default:
                return ::DefWindowProcW(h, msg, wp, lp);
            }
        }
    };
    std::unique_ptr<SettingsHost> g_settings;
}

extern "C" __declspec(dllexport) HRESULT __stdcall OpenShazamSettingsShow(HWND owner) noexcept
{
    try
    {
        if (!owner || !::IsWindow(owner)) return E_INVALIDARG;
        if (!g_settings)
        {
            auto host = std::make_unique<SettingsHost>();
            g_active = host.get();
            try { host->Initialize(owner); }
            catch (...) { host->Close(); g_active = nullptr; throw; }
            g_settings = std::move(host);
        }
        g_settings->Show();
        return S_OK;
    }
    catch (winrt::hresult_error const& e) { return e.code(); }
    catch (...) { return E_FAIL; }
}

extern "C" __declspec(dllexport) BOOL __stdcall OpenShazamSettingsPreTranslate(MSG* msg) noexcept
{
    if (!g_settings || !g_settings->hwnd || !msg) return FALSE;
    return ::ContentPreTranslateMessage(msg) ? TRUE : FALSE;
}

extern "C" __declspec(dllexport) BOOL __stdcall OpenShazamSettingsIsVisible() noexcept
{
    return g_settings && g_settings->hwnd && ::IsWindowVisible(g_settings->hwnd);
}

extern "C" __declspec(dllexport) void __stdcall OpenShazamSettingsShutdown() noexcept
{
    if (g_settings)
    {
        g_settings->Close();
        g_settings.reset();
        g_active = nullptr;
    }
}
