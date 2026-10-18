// WinUI 3 Settings XAML Island hosted IN the native Rust/Win32 process.
// No second recognition controller, process, webview, or legacy Win32 settings UI.
#include "pch.h"
#include <winrt/Windows.Foundation.Collections.h>

namespace x = winrt::Microsoft::UI::Xaml;
namespace controls = winrt::Microsoft::UI::Xaml::Controls;
namespace hosting = winrt::Microsoft::UI::Xaml::Hosting;
namespace dispatch = winrt::Microsoft::UI::Dispatching;
namespace markup = winrt::Microsoft::UI::Xaml::Markup;
namespace typeinfo = winrt::Microsoft::UI::Xaml::XamlTypeInfo;

namespace
{
    // No App.xaml, XAML compiler-generated ABI or a second app process is
    // needed. ApplicationT provides the metadata interface required by the
    // built-in Fluent WinUI 3 controls.
    struct SettingsApplication : x::ApplicationT<SettingsApplication, markup::IXamlMetadataProvider>
    {
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
        typeinfo::XamlControlsXamlMetaDataProvider provider;
    };

    constexpr wchar_t kSettingsClass[] = L"OpenShazamWinUI3Preferences";
    constexpr wchar_t kRunKey[] = L"Software\\Microsoft\\Windows\\CurrentVersion\\Run";
    constexpr wchar_t kRunValue[] = L"OpenShazam";
    constexpr int kWidthDips = 520;
    constexpr int kHeightDips = 300;

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
            info.Severity(level);
            info.Message(message);
            info.IsOpen(true);
        }
    };

    // An unpackaged, per-user login entry; does not auto-start recognition.
    std::wstring StartupCommand()
    {
        std::wstring path(260, L'\0');
        for (;;)
        {
            auto length = ::GetModuleFileNameW(nullptr, path.data(),
                static_cast<DWORD>(path.size()));
            if (!length) winrt::throw_last_error();
            if (length < path.size())
            {
                path.resize(length);
                break;
            }
            if (path.size() >= 32768)
                winrt::throw_hresult(HRESULT_FROM_WIN32(ERROR_FILENAME_EXCED_RANGE));
            path.resize(path.size() * 2);
        }
        return L"\"" + path + L"\" ui";
    }

    void CheckRegistryStatus(LSTATUS status)
    {
        if (status != ERROR_SUCCESS)
            winrt::throw_hresult(HRESULT_FROM_WIN32(status));
    }

    bool IsStartupEnabled()
    {
        DWORD bytes{};
        auto status = ::RegGetValueW(HKEY_CURRENT_USER, kRunKey, kRunValue,
            RRF_RT_REG_SZ, nullptr, nullptr, &bytes);
        if (status == ERROR_FILE_NOT_FOUND || status == ERROR_PATH_NOT_FOUND)
            return false;
        CheckRegistryStatus(status);
        std::wstring command(bytes / sizeof(wchar_t), L'\0');
        status = ::RegGetValueW(HKEY_CURRENT_USER, kRunKey, kRunValue,
            RRF_RT_REG_SZ, nullptr, command.data(), &bytes);
        CheckRegistryStatus(status);
        command.resize(command.find(L'\0'));
        return command == StartupCommand();
    }

    void SetStartupEnabled(bool enabled)
    {
        HKEY key{};
        if (enabled)
        {
            auto command = StartupCommand();
            CheckRegistryStatus(::RegCreateKeyExW(HKEY_CURRENT_USER, kRunKey,
                0, nullptr, REG_OPTION_NON_VOLATILE, KEY_SET_VALUE, nullptr,
                &key, nullptr));
            auto status = ::RegSetValueExW(key, kRunValue, 0, REG_SZ,
                reinterpret_cast<BYTE const*>(command.c_str()),
                static_cast<DWORD>((command.size() + 1) * sizeof(wchar_t)));
            ::RegCloseKey(key);
            CheckRegistryStatus(status);
        }
        else
        {
            auto status = ::RegOpenKeyExW(HKEY_CURRENT_USER, kRunKey, 0,
                KEY_SET_VALUE, &key);
            if (status == ERROR_FILE_NOT_FOUND || status == ERROR_PATH_NOT_FOUND)
                return;
            CheckRegistryStatus(status);
            status = ::RegDeleteValueW(key, kRunValue);
            ::RegCloseKey(key);
            if (status != ERROR_FILE_NOT_FOUND)
                CheckRegistryStatus(status);
        }
    }

    void RefreshStartup(std::weak_ptr<SettingsState> weak)
    {
        auto state = weak.lock();
        if (!state || !state->alive) return;
        state->loading = true;
        state->toggle.IsEnabled(false);
        try
        {
            state->toggle.IsOn(IsStartupEnabled());
            state->toggle.IsEnabled(true);
            state->info.IsOpen(false);
        }
        catch (winrt::hresult_error const&)
        {
            state->toggle.IsOn(false);
            state->Status(Localized(
                L"Windows could not read the startup setting.",
                L"Windowsの自動起動設定を読み取れませんでした。"),
                controls::InfoBarSeverity::Error);
        }
        state->loading = false;
    }

    void ChangeStartup(std::weak_ptr<SettingsState> weak, bool enable)
    {
        auto state = weak.lock();
        if (!state || !state->alive) return;
        state->loading = true;
        state->toggle.IsEnabled(false);
        bool failed = false;
        try
        {
            SetStartupEnabled(enable);
        }
        catch (winrt::hresult_error const&)
        {
            failed = true;
        }
        if (!state->alive) return;
        RefreshStartup(weak);
        if (failed && state->alive)
            state->Status(Localized(
                L"Windows could not update the startup setting.",
                L"Windowsで自動起動の設定を変更できませんでした。"),
                controls::InfoBarSeverity::Error);
    }

    struct SettingsHost;
    SettingsHost* g_active{};

    struct SettingsHost
    {
        HWND hwnd{};
        HWND owner{};
        hosting::DesktopWindowXamlSource island{ nullptr };
        dispatch::DispatcherQueueController queue{ nullptr };
        bool ownsQueue{};
        x::Application app{ nullptr };
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
            app = winrt::make<SettingsApplication>();
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
            hwnd = ::CreateWindowExW(0, kSettingsClass, L"OpenShazam Settings",
                WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN, CW_USEDEFAULT, CW_USEDEFAULT,
                ::MulDiv(kWidthDips, dpi, 96), ::MulDiv(kHeightDips, dpi, 96),
                parent, nullptr, instance, nullptr);
            if (!hwnd) winrt::throw_last_error();
            island = hosting::DesktopWindowXamlSource{};
            island.Initialize(winrt::Microsoft::UI::GetWindowIdFromWindow(hwnd));
            app.Resources().MergedDictionaries().Append(controls::XamlControlsResources{});
            auto child = winrt::Microsoft::UI::GetWindowFromWindowId(island.SiteBridge().WindowId());
            auto style = ::GetWindowLongPtrW(child, GWL_STYLE);
            ::SetWindowLongPtrW(child, GWL_STYLE, style | WS_TABSTOP | WS_VISIBLE);
            state = std::make_shared<SettingsState>();
            island.Content(BuildContent());
            Layout();
            RefreshStartup(state);
        }

        x::UIElement BuildContent()
        {
            controls::StackPanel stack;
            stack.Spacing(18);
            stack.Padding(x::ThicknessHelper::FromUniformLength(24));

            controls::TextBlock title;
            title.Text(Localized(L"Settings", L"設定"));
            title.FontSize(26);
            title.FontWeight(winrt::Windows::UI::Text::FontWeights::SemiBold());
            stack.Children().Append(title);

            controls::TextBlock description;
            description.Text(Localized(
                L"Manage how OpenShazam starts and behaves.",
                L"OpenShazamの起動方法を設定します。"));
            description.TextWrapping(x::TextWrapping::Wrap);
            stack.Children().Append(description);

            state->toggle = controls::ToggleSwitch{};
            state->toggle.Header(winrt::box_value(Localized(
                L"Launch OpenShazam when I sign in",
                L"Windowsへのサインイン時にOpenShazamを起動")));
            state->toggle.OnContent(winrt::box_value(Localized(L"On", L"オン")));
            state->toggle.OffContent(winrt::box_value(Localized(L"Off", L"オフ")));
            state->toggle.IsEnabled(false);
            std::weak_ptr<SettingsState> weak = state;
            state->toggle.Toggled([weak](auto const&, auto const&) {
                if (auto state = weak.lock(); state && state->alive && !state->loading)
                    ChangeStartup(weak, state->toggle.IsOn());
            });
            stack.Children().Append(state->toggle);

            controls::TextBlock note;
            note.Text(Localized(
                L"OpenShazam starts in the notification area; recognition does not start automatically.",
                L"OpenShazamは通知領域で起動します。楽曲認識は自動では開始しません。"));
            note.TextWrapping(x::TextWrapping::Wrap);
            stack.Children().Append(note);

            state->info = controls::InfoBar{};
            state->info.IsOpen(false);
            state->info.IsClosable(false);
            stack.Children().Append(state->info);

            controls::ScrollViewer viewer;
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
            ::SetForegroundWindow(hwnd);
            ::SetFocus(winrt::Microsoft::UI::GetWindowFromWindowId(island.SiteBridge().WindowId()));
            if (state) RefreshStartup(state);
        }

        void Close() noexcept
        {
            try
            {
                if (state) state->alive = false;
                if (island) island.Close();
                island = nullptr;
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
