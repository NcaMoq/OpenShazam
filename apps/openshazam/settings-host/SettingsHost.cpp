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
namespace viewmanagement = winrt::Windows::UI::ViewManagement;

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
    constexpr UINT WM_APPEARANCE_CHANGED = WM_APP + 0x48;
    constexpr uint32_t kAppearanceSystem = 0;
    constexpr uint32_t kAppearanceLight = 1;
    constexpr uint32_t kAppearanceDark = 2;
    constexpr int kWidthDips = 520;
    constexpr int kHeightDips = 380;

    bool SystemUsesDarkAppearance() noexcept
    {
        try
        {
            viewmanagement::UISettings settings;
            auto foreground = settings.GetColorValue(viewmanagement::UIColorType::Foreground);
            const float luminance = 0.2126f * foreground.R +
                                    0.7152f * foreground.G +
                                    0.0722f * foreground.B;
            return luminance >= 0.5f;
        }
        catch (...)
        {
            return false;
        }
    }

    void ApplyTitleBarTheme(HWND window, uint32_t appearanceMode) noexcept
    {
        if (!window) return;
        BOOL useDarkMode = appearanceMode == kAppearanceSystem
            ? (SystemUsesDarkAppearance() ? TRUE : FALSE)
            : (appearanceMode == kAppearanceDark ? TRUE : FALSE);
        auto setAttribute = [&](DWORD attribute) {
            return SUCCEEDED(::DwmSetWindowAttribute(
                window,
                static_cast<DWMWINDOWATTRIBUTE>(attribute),
                &useDarkMode,
                sizeof(useDarkMode)));
        };
        // Attribute 20 is the documented Windows 11 value. Windows 10
        // 1903 used value 19 before the public SDK constant was added.
        if (!setAttribute(20)) setAttribute(19);
    }

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
        controls::ComboBox appearance{ nullptr };
        controls::ScrollViewer viewer{ nullptr };
        controls::Border startupCard{ nullptr };
        controls::Border appearanceCard{ nullptr };
        controls::Grid startupRow{ nullptr };
        controls::Grid appearanceRow{ nullptr };
        HWND owner{};
        HWND hwnd{};
        uint32_t appearanceMode{ kAppearanceSystem };
        bool startupLoading{ true };
        bool appearanceLoading{ true };
        bool alive{ true };
    };

    int ComboIndexFromAppearance(uint32_t mode) noexcept
    {
        switch (mode)
        {
        case kAppearanceLight: return 0;
        case kAppearanceDark: return 1;
        default: return 2;
        }
    }

    uint32_t AppearanceFromComboIndex(int index) noexcept
    {
        switch (index)
        {
        case 0: return kAppearanceLight;
        case 1: return kAppearanceDark;
        default: return kAppearanceSystem;
        }
    }

    void ApplyAppearanceToSettings(std::shared_ptr<SettingsState> const& state, uint32_t mode)
    {
        if (!state || !state->alive) return;
        if (mode > kAppearanceDark) mode = kAppearanceSystem;
        state->appearanceMode = mode;
        if (state->viewer)
        {
            auto theme = mode == kAppearanceDark ? x::ElementTheme::Dark :
                mode == kAppearanceLight ? x::ElementTheme::Light : x::ElementTheme::Default;
            state->viewer.RequestedTheme(theme);
        }
        ApplyTitleBarTheme(state->hwnd, mode);
    }

    void DisableStartupControlNoThrow(std::shared_ptr<SettingsState> const& state) noexcept
    {
        if (!state || !state->alive) return;
        state->startupLoading = false;
        try
        {
            state->toggle.IsOn(false);
            state->toggle.IsEnabled(false);
        }
        catch (...)
        {
            // WinRT property updates can fail while the island is closing.
            // Never allow an exception to escape a fire_and_forget coroutine.
        }
    }

    winrt::fire_and_forget RefreshStartup(std::weak_ptr<SettingsState> weak)
    {
        auto state = weak.lock();
        if (!state || !state->alive) co_return;
        try
        {
            state->startupLoading = true;
            state->toggle.IsEnabled(false);
            auto task = co_await appmodel::StartupTask::GetAsync(kStartupTask);
            if (!state->alive) co_return;
            auto current = task.State();
            state->toggle.IsOn(current == appmodel::StartupTaskState::Enabled ||
                               current == appmodel::StartupTaskState::EnabledByPolicy);
            state->toggle.IsEnabled(current == appmodel::StartupTaskState::Disabled ||
                                    current == appmodel::StartupTaskState::Enabled);
            state->startupLoading = false;
        }
        catch (winrt::hresult_error const&)
        {
            DisableStartupControlNoThrow(state);
        }
        catch (...)
        {
            DisableStartupControlNoThrow(state);
        }
    }

    winrt::fire_and_forget ChangeStartup(std::weak_ptr<SettingsState> weak, bool enable)
    {
        auto state = weak.lock();
        if (!state || !state->alive) co_return;
        try
        {
            state->startupLoading = true;
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
            DisableStartupControlNoThrow(state);
        }
        catch (...)
        {
            DisableStartupControlNoThrow(state);
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
            ApplyTitleBarTheme(hwnd, kAppearanceSystem);
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
            state->owner = owner;
            state->hwnd = hwnd;
            island.Content(BuildContent());
            Layout();
            RefreshStartup(state);
        }

        x::UIElement BuildContent()
        {
            controls::StackPanel stack;
            stack.Spacing(8);
            stack.Padding(x::ThicknessHelper::FromUniformLength(24));

            state->startupCard = markup::XamlReader::Load(
                LR"(<Border xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation" Background="{ThemeResource CardBackgroundFillColorDefaultBrush}" CornerRadius="8" Padding="14" />)"
            ).as<controls::Border>();
            state->startupRow = controls::Grid{};
            auto startupRow = state->startupRow;
            startupRow.HorizontalAlignment(x::HorizontalAlignment::Stretch);
            auto startupLabelColumn = controls::ColumnDefinition{};
            startupLabelColumn.Width(x::GridLengthHelper::FromValueAndType(1.0, x::GridUnitType::Star));
            auto startupToggleColumn = controls::ColumnDefinition{};
            startupToggleColumn.Width(x::GridLengthHelper::FromValueAndType(150.0, x::GridUnitType::Pixel));
            startupRow.ColumnDefinitions().Append(startupLabelColumn);
            startupRow.ColumnDefinitions().Append(startupToggleColumn);

            controls::TextBlock startupLabel;
            state->toggle = controls::ToggleSwitch{};
            auto startupName = Localized(L"Start on sign-in", L"自動起動");
            startupLabel.Text(startupName);
            startupLabel.FontSize(14.0);
            startupLabel.HorizontalAlignment(x::HorizontalAlignment::Left);
            startupLabel.VerticalAlignment(x::VerticalAlignment::Center);
            automation::AutomationProperties::SetName(startupLabel, startupName);
            controls::Grid::SetColumn(startupLabel, 0);
            startupRow.Children().Append(startupLabel);

            automation::AutomationProperties::SetName(state->toggle, startupName);
            state->toggle.OnContent(winrt::box_value(Localized(L"On", L"オン")));
            state->toggle.OffContent(winrt::box_value(Localized(L"Off", L"オフ")));
            state->toggle.VerticalAlignment(x::VerticalAlignment::Center);
            state->toggle.HorizontalAlignment(x::HorizontalAlignment::Right);
            state->toggle.IsEnabled(false);
            std::weak_ptr<SettingsState> weak = state;
            state->toggle.Toggled([weak](auto const&, auto const&) {
                if (auto state = weak.lock(); state && state->alive && !state->startupLoading)
                    ChangeStartup(weak, state->toggle.IsOn());
            });
            controls::Grid::SetColumn(state->toggle, 1);
            startupRow.Children().Append(state->toggle);
            state->startupCard.Child(startupRow);
            stack.Children().Append(state->startupCard);

            state->appearanceCard = markup::XamlReader::Load(
                LR"(<Border xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation" Background="{ThemeResource CardBackgroundFillColorDefaultBrush}" CornerRadius="8" Padding="14" />)"
            ).as<controls::Border>();
            state->appearanceRow = controls::Grid{};
            auto appearanceRow = state->appearanceRow;
            appearanceRow.HorizontalAlignment(x::HorizontalAlignment::Stretch);
            auto iconColumn = controls::ColumnDefinition{};
            iconColumn.Width(x::GridLengthHelper::FromValueAndType(34.0, x::GridUnitType::Pixel));
            auto labelColumn = controls::ColumnDefinition{};
            labelColumn.Width(x::GridLengthHelper::FromValueAndType(1.0, x::GridUnitType::Star));
            auto selectorColumn = controls::ColumnDefinition{};
            selectorColumn.Width(x::GridLengthHelper::FromValueAndType(208.0, x::GridUnitType::Pixel));
            appearanceRow.ColumnDefinitions().Append(iconColumn);
            appearanceRow.ColumnDefinitions().Append(labelColumn);
            appearanceRow.ColumnDefinitions().Append(selectorColumn);

            controls::FontIcon appearanceIcon;
            appearanceIcon.Glyph(L"\uE790");
            appearanceIcon.FontSize(20.0);
            appearanceIcon.HorizontalAlignment(x::HorizontalAlignment::Left);
            appearanceIcon.VerticalAlignment(x::VerticalAlignment::Center);
            controls::Grid::SetColumn(appearanceIcon, 0);
            appearanceRow.Children().Append(appearanceIcon);

            controls::StackPanel appearanceLabels;
            appearanceLabels.Spacing(2.0);
            appearanceLabels.HorizontalAlignment(x::HorizontalAlignment::Left);
            appearanceLabels.VerticalAlignment(x::VerticalAlignment::Center);
            controls::TextBlock appearanceTitle;
            auto appearanceName = Localized(L"Choose a mode", L"モードを選ぶ");
            appearanceTitle.Text(appearanceName);
            appearanceTitle.FontSize(14.0);
            appearanceTitle.TextAlignment(x::TextAlignment::Left);
            automation::AutomationProperties::SetName(appearanceTitle, appearanceName);
            controls::TextBlock appearanceDescription;
            appearanceDescription.Text(Localized(
                L"Changes the colors displayed in Windows and apps",
                L"Windows とアプリに表示される色を変更します"));
            appearanceDescription.FontSize(12.0);
            appearanceDescription.TextAlignment(x::TextAlignment::Left);
            appearanceLabels.Children().Append(appearanceTitle);
            appearanceLabels.Children().Append(appearanceDescription);
            controls::Grid::SetColumn(appearanceLabels, 1);
            appearanceRow.Children().Append(appearanceLabels);

            state->appearance = controls::ComboBox{};
            state->appearance.Items().Append(winrt::box_value(Localized(L"Light", L"ライト")));
            state->appearance.Items().Append(winrt::box_value(Localized(L"Dark", L"ダーク")));
            state->appearance.Items().Append(winrt::box_value(Localized(
                L"Use system setting", L"システム設定に合わせる")));
            state->appearance.Width(208.0);
            state->appearance.HorizontalAlignment(x::HorizontalAlignment::Right);
            state->appearance.VerticalAlignment(x::VerticalAlignment::Center);
            automation::AutomationProperties::SetName(state->appearance, appearanceName);
            state->appearance.SelectedIndex(ComboIndexFromAppearance(state->appearanceMode));
            state->appearanceLoading = false;
            state->appearance.SelectionChanged([weak](auto const&, auto const&) {
                auto state = weak.lock();
                if (!state || !state->alive || state->appearanceLoading) return;
                const uint32_t mode = AppearanceFromComboIndex(state->appearance.SelectedIndex());
                ApplyAppearanceToSettings(state, mode);
                ::PostMessageW(state->owner, WM_APPEARANCE_CHANGED,
                    static_cast<WPARAM>(mode), 0);
            });
            controls::Grid::SetColumn(state->appearance, 2);
            appearanceRow.Children().Append(state->appearance);
            state->appearanceCard.Child(appearanceRow);
            stack.Children().Append(state->appearanceCard);

            // Keep the background as a XAML ThemeResource expression rather
            // than copying the current brush instance. That lets the root
            // switch palettes when Windows changes app appearance at runtime.
            state->viewer = markup::XamlReader::Load(
                LR"(<ScrollViewer xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation" Background="{ThemeResource ApplicationPageBackgroundThemeBrush}" />)"
            ).as<controls::ScrollViewer>();
            state->viewer.Content(stack);
            state->viewer.VerticalScrollBarVisibility(controls::ScrollBarVisibility::Auto);
            state->viewer.HorizontalScrollBarVisibility(controls::ScrollBarVisibility::Disabled);
            ApplyAppearanceToSettings(state, state->appearanceMode);
            return state->viewer;
        }

        void Layout()
        {
            if (!hwnd || !island) return;
            RECT rect{};
            ::GetClientRect(hwnd, &rect);
            auto dpi = ::GetDpiForWindow(hwnd);
            if (dpi < 96) dpi = 96;
            if (state && state->startupCard && state->appearanceCard &&
                state->startupRow && state->appearanceRow)
            {
                const double cardWidth = ::MulDiv(rect.right - rect.left, 96, dpi) - 48.0;
                if (cardWidth > 28.0)
                {
                    state->startupCard.Width(cardWidth);
                    state->appearanceCard.Width(cardWidth);
                    // Border padding consumes 14 DIPs on each side. Pin each
                    // row to the remaining content width so the trailing
                    // control stays at the card's right edge even when the
                    // island is measured before its host window is shown.
                    const double rowWidth = cardWidth - 28.0;
                    state->startupRow.Width(rowWidth);
                    state->appearanceRow.Width(rowWidth);
                }
            }
            island.SiteBridge().MoveAndResize({0, 0, rect.right - rect.left, rect.bottom - rect.top});
        }

        void Show(uint32_t appearanceMode)
        {
            if (!hwnd) return;
            if (state)
            {
                state->appearanceLoading = true;
                state->appearance.SelectedIndex(ComboIndexFromAppearance(appearanceMode));
                state->appearanceLoading = false;
                ApplyAppearanceToSettings(state, appearanceMode);
            }
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
            case WM_SETTINGCHANGE:
            case WM_THEMECHANGED:
                if (host && host->hwnd == h && host->state)
                    ApplyAppearanceToSettings(host->state, host->state->appearanceMode);
                return ::DefWindowProcW(h, msg, wp, lp);
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

extern "C" __declspec(dllexport) HRESULT __stdcall OpenShazamSettingsShow(
    HWND owner, uint32_t appearanceMode) noexcept
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
        g_settings->Show(appearanceMode);
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
