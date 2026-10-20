// WinUI 3 Settings XAML Island hosted IN the native Rust/Win32 process.
// No second recognition controller, process, webview, or legacy Win32 settings UI.
#include "pch.h"
#include <shellapi.h>
#include <algorithm>
#include <cwchar>
#include <iterator>
#include <stdexcept>
#include <string>
#include <system_error>
#include <vector>
#include <winrt/Windows.Foundation.Collections.h>
#include <winrt/Windows.System.h>
#include <winrt/Windows.UI.h>
#include <winrt/Microsoft.UI.Windowing.h>
#include <winrt/Microsoft.UI.Xaml.Automation.h>
#include <winrt/Microsoft.UI.Xaml.Input.h>

#pragma comment(lib, "Advapi32.lib")
#pragma comment(lib, "Shell32.lib")

struct OpenShazamHistoryItem
{
    wchar_t const* title;
    wchar_t const* artist;
    wchar_t const* recognizedAt;
    wchar_t const* pageUrl;
};

namespace x = winrt::Microsoft::UI::Xaml;
namespace controls = winrt::Microsoft::UI::Xaml::Controls;
namespace media = winrt::Microsoft::UI::Xaml::Media;
namespace hosting = winrt::Microsoft::UI::Xaml::Hosting;
namespace dispatch = winrt::Microsoft::UI::Dispatching;
namespace automation = winrt::Microsoft::UI::Xaml::Automation;
namespace markup = winrt::Microsoft::UI::Xaml::Markup;
namespace typeinfo = winrt::Microsoft::UI::Xaml::XamlTypeInfo;

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
    constexpr UINT WM_APPEARANCE_CHANGED = WM_APP + 0x48;
    constexpr UINT WM_GLOBAL_SHORTCUT_CHANGED = WM_APP + 0x4a;
    constexpr UINT WM_GLOBAL_SHORTCUT_CAPTURE = WM_APP + 0x4b;
    constexpr UINT WM_LANGUAGE_CHANGED = WM_APP + 0x4c;
    constexpr uint32_t kAppearanceSystem = 0;
    constexpr uint32_t kAppearanceLight = 1;
    constexpr uint32_t kAppearanceDark = 2;
    constexpr uint32_t kLanguageJapanese = 1;
    constexpr uint32_t kLanguageEnglish = 2;
    // WinUI's left-navigation silhouette reserves 56 epx between the pane
    // and page content. Keep the content on that grid at every window size.
    constexpr double kSettingsContentGutterDips = 56.0;
    constexpr double kSettingsCardPaddingDips = 16.0;
    constexpr double kSettingsSectionSpacingDips = 12.0;
    constexpr double kSettingsInlineMessageSpacingDips = 8.0;
    constexpr double kSettingsControlLaneWidthDips = 208.0;
    constexpr double kAppearanceSelectorWidthDips = 120.0;
    constexpr double kStartupControlGroupWidthDips = 94.0;
    constexpr int kWidthDips = 1040;
    constexpr int kHeightDips = 680;
    constexpr int kMinimumClientWidthDips = 800;
    constexpr int kMinimumClientHeightDips = 480;
    constexpr wchar_t kStartupRunKey[] = L"Software\\Microsoft\\Windows\\CurrentVersion\\Run";
    constexpr wchar_t kStartupRunValue[] = L"OpenShazam";
    uint32_t g_uiLanguage{};

    [[noreturn]] void ThrowRegistryError(LSTATUS error, char const* operation)
    {
        throw std::system_error(static_cast<int>(error), std::system_category(), operation);
    }

    struct RunRegistryKey
    {
        HKEY value{};
        ~RunRegistryKey() { if (value) ::RegCloseKey(value); }
    };

    std::wstring CurrentExecutablePath()
    {
        std::vector<wchar_t> buffer(512);
        for (;;)
        {
            const DWORD length = ::GetModuleFileNameW(nullptr, buffer.data(),
                static_cast<DWORD>(buffer.size()));
            if (length == 0)
                throw std::system_error(static_cast<int>(::GetLastError()),
                    std::system_category(), "GetModuleFileNameW failed");
            if (length < buffer.size())
                return std::wstring(buffer.data(), length);
            if (buffer.size() >= 32768)
                throw std::length_error("OpenShazam executable path exceeds the Windows limit");
            buffer.resize((std::min)(buffer.size() * 2, size_t{32768}));
        }
    }

    std::wstring StartupCommand()
    {
        return L"\"" + CurrentExecutablePath() + L"\" ui --startup";
    }

    bool HasMatchingRunStartupValue()
    {
        RunRegistryKey key;
        LSTATUS status = ::RegOpenKeyExW(HKEY_CURRENT_USER, kStartupRunKey, 0,
            KEY_QUERY_VALUE, &key.value);
        if (status == ERROR_FILE_NOT_FOUND) return false;
        if (status != ERROR_SUCCESS) ThrowRegistryError(status, "RegOpenKeyExW failed");

        DWORD type = 0;
        DWORD byteCount = 0;
        status = ::RegQueryValueExW(key.value, kStartupRunValue, nullptr, &type,
            nullptr, &byteCount);
        if (status == ERROR_FILE_NOT_FOUND) return false;
        if (status != ERROR_SUCCESS) ThrowRegistryError(status, "RegQueryValueExW failed");
        if (type != REG_SZ || byteCount < sizeof(wchar_t) || byteCount % sizeof(wchar_t) != 0)
            throw std::runtime_error("The Shazam startup entry is not a valid REG_SZ value.");

        std::vector<wchar_t> value(byteCount / sizeof(wchar_t), L'\0');
        status = ::RegQueryValueExW(key.value, kStartupRunValue, nullptr, &type,
            reinterpret_cast<BYTE*>(value.data()), &byteCount);
        if (status != ERROR_SUCCESS) ThrowRegistryError(status, "RegQueryValueExW failed");
        if (value.empty() || value.back() != L'\0' ||
            std::find(value.begin(), value.end() - 1, L'\0') != value.end() - 1)
            throw std::runtime_error("The Shazam startup entry has malformed quoting or termination.");
        const std::wstring actual(value.begin(), value.end() - 1);
        const std::wstring expected = StartupCommand();
        if (::CompareStringOrdinal(actual.c_str(), -1, expected.c_str(), -1, TRUE) != CSTR_EQUAL)
            throw std::runtime_error("The Shazam startup entry does not match this installation.");
        return true;
    }

    bool StartupDisabledInWindows()
    {
        constexpr wchar_t kStartupApprovedKey[] =
            L"Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved\\Run";
        RunRegistryKey key;
        LSTATUS status = ::RegOpenKeyExW(HKEY_CURRENT_USER, kStartupApprovedKey, 0,
            KEY_QUERY_VALUE, &key.value);
        if (status == ERROR_FILE_NOT_FOUND) return false;
        if (status != ERROR_SUCCESS) ThrowRegistryError(status, "RegOpenKeyExW failed");

        DWORD type = 0;
        DWORD byteCount = 0;
        status = ::RegQueryValueExW(key.value, kStartupRunValue, nullptr, &type,
            nullptr, &byteCount);
        if (status == ERROR_FILE_NOT_FOUND) return false;
        if (status != ERROR_SUCCESS) ThrowRegistryError(status, "RegQueryValueExW failed");
        if (type != REG_BINARY || byteCount == 0)
            throw std::runtime_error("Windows returned an invalid startup approval state.");

        std::vector<BYTE> state(byteCount);
        status = ::RegQueryValueExW(key.value, kStartupRunValue, nullptr, &type,
            state.data(), &byteCount);
        if (status != ERROR_SUCCESS) ThrowRegistryError(status, "RegQueryValueExW failed");
        if (state[0] == 0x03) return true;
        if (state[0] == 0x02) return false;
        throw std::runtime_error("Windows returned an unknown startup approval state.");
    }

    struct StartupStatus
    {
        bool enabled{};
        bool canChange{ true };
        bool disabledByWindows{};
    };

    StartupStatus ReadStartupStatus()
    {
        if (!HasMatchingRunStartupValue()) return {};
        if (StartupDisabledInWindows()) return { false, false, true };
        return { true, true, false };
    }

    void SetRunStartupEnabled(bool enable)
    {
        RunRegistryKey key;
        LSTATUS status = ERROR_SUCCESS;
        if (enable)
        {
            if (StartupDisabledInWindows())
                throw std::runtime_error("Windows has disabled Shazam in Startup apps.");
            if (HasMatchingRunStartupValue()) return;
            DWORD disposition = 0;
            status = ::RegCreateKeyExW(HKEY_CURRENT_USER, kStartupRunKey, 0, nullptr,
                REG_OPTION_NON_VOLATILE, KEY_SET_VALUE, nullptr, &key.value, &disposition);
            if (status != ERROR_SUCCESS) ThrowRegistryError(status, "RegCreateKeyExW failed");
            const std::wstring command = StartupCommand();
            const DWORD byteCount = static_cast<DWORD>((command.size() + 1) * sizeof(wchar_t));
            status = ::RegSetValueExW(key.value, kStartupRunValue, 0, REG_SZ,
                reinterpret_cast<BYTE const*>(command.c_str()), byteCount);
            if (status != ERROR_SUCCESS) ThrowRegistryError(status, "RegSetValueExW failed");
            return;
        }

        status = ::RegOpenKeyExW(HKEY_CURRENT_USER, kStartupRunKey, 0,
            KEY_SET_VALUE, &key.value);
        if (status == ERROR_FILE_NOT_FOUND) return;
        if (status != ERROR_SUCCESS) ThrowRegistryError(status, "RegOpenKeyExW failed");
        if (!HasMatchingRunStartupValue()) return;
        status = ::RegDeleteValueW(key.value, kStartupRunValue);
        if (status != ERROR_SUCCESS && status != ERROR_FILE_NOT_FOUND)
            ThrowRegistryError(status, "RegDeleteValueW failed");
    }

    bool IsJapanese() noexcept
    {
        if (g_uiLanguage == kLanguageJapanese) return true;
        if (g_uiLanguage == kLanguageEnglish) return false;
        return PRIMARYLANGID(::GetUserDefaultUILanguage()) == LANG_JAPANESE;
    }

    uint32_t LanguageFromComboIndex(int32_t index) noexcept
    {
        return index == 0 ? kLanguageJapanese : kLanguageEnglish;
    }

    int32_t ComboIndexFromLanguage(uint32_t language) noexcept
    {
        return language == kLanguageJapanese ? 0 : 1;
    }

    winrt::hstring Localized(wchar_t const* en, wchar_t const* ja)
    {
        return IsJapanese() ? winrt::hstring(ja) : winrt::hstring(en);
    }

    winrt::Windows::Foundation::Uri GetTitleBarIconUri()
    {
        std::wstring executablePath(32768, L'\0');
        const auto length = ::GetModuleFileNameW(nullptr,
            executablePath.data(), static_cast<DWORD>(executablePath.size()));
        if (length == 0 || length >= executablePath.size())
            winrt::throw_last_error();
        executablePath.resize(length);
        const auto separator = executablePath.find_last_of(L"\\/");
        if (separator == std::wstring::npos)
            throw std::runtime_error("Could not resolve the app icon path.");

        executablePath.resize(separator + 1);
        executablePath += L"OpenShazamSquare44.png";
        std::replace(executablePath.begin(), executablePath.end(), L'\\', L'/');
        return winrt::Windows::Foundation::Uri(
            winrt::hstring(L"file:///" + executablePath));
    }

    std::wstring FormatShortcut(uint32_t modifiers, uint32_t key)
    {
        std::wstring label;
        auto append = [&](wchar_t const* part) {
            if (!label.empty()) label += L" + ";
            label += part;
        };
        if (modifiers & MOD_CONTROL) append(L"Ctrl");
        if (modifiers & MOD_ALT) append(L"Alt");
        if (modifiers & MOD_SHIFT) append(L"Shift");
        if (modifiers & MOD_WIN) append(L"Win");

        wchar_t keyName[128]{};
        const auto scanCode = ::MapVirtualKeyW(key, MAPVK_VK_TO_VSC);
        LONG keyNameParam = static_cast<LONG>(scanCode << 16);
        if (key == VK_LEFT || key == VK_UP || key == VK_RIGHT || key == VK_DOWN ||
            key == VK_PRIOR || key == VK_NEXT || key == VK_END || key == VK_HOME ||
            key == VK_INSERT || key == VK_DELETE || key == VK_DIVIDE || key == VK_NUMLOCK)
            keyNameParam |= 1L << 24;
        if (::GetKeyNameTextW(keyNameParam, keyName, static_cast<int>(std::size(keyName))) == 0)
            std::swprintf(keyName, std::size(keyName), L"0x%02X", key);
        append(keyName);
        return label;
    }

    struct HistoryRow
    {
        std::wstring title;
        std::wstring artist;
        std::wstring recognizedAt;
        std::wstring pageUrl;
    };

    struct SettingsState
    {
        controls::ToggleSwitch toggle{ nullptr };
        controls::TextBlock startupValue{ nullptr };
        controls::TextBlock settingsPageTitle{ nullptr };
        controls::TextBlock startupTitle{ nullptr };
        controls::TextBlock startupDescription{ nullptr };
        controls::TextBlock appearanceTitle{ nullptr };
        controls::TextBlock appearanceDescription{ nullptr };
        controls::TextBlock languageTitle{ nullptr };
        controls::TextBlock languageDescription{ nullptr };
        controls::TextBlock shortcutTitle{ nullptr };
        controls::TextBlock shortcutDescription{ nullptr };
        controls::InfoBar startupInfoBar{ nullptr };
        controls::ComboBox appearance{ nullptr };
        controls::ComboBox language{ nullptr };
        controls::Grid shellRoot{ nullptr };
        controls::TitleBar titleBar{ nullptr };
        controls::NavigationView navigation{ nullptr };
        controls::Button shortcutButton{ nullptr };
        controls::InfoBar shortcutInfoBar{ nullptr };
        controls::ScrollViewer settingsViewer{ nullptr };
        controls::ScrollViewer historyViewer{ nullptr };
        controls::Grid pageHost{ nullptr };
        controls::StackPanel settingsContent{ nullptr };
        controls::StackPanel historyContent{ nullptr };
        controls::StackPanel historyItems{ nullptr };
        controls::TextBlock historyEmpty{ nullptr };
        controls::TextBlock historyTitle{ nullptr };
        controls::NavigationViewItem historyNavigationItem{ nullptr };
        winrt::Microsoft::UI::Windowing::AppWindowTitleBar appWindowTitleBar{ nullptr };
        std::vector<HistoryRow> historySnapshot;
        controls::Border startupCard{ nullptr };
        controls::Border appearanceCard{ nullptr };
        controls::Border languageCard{ nullptr };
        controls::Border shortcutCard{ nullptr };
        controls::Grid startupRow{ nullptr };
        controls::Grid appearanceRow{ nullptr };
        controls::Grid languageRow{ nullptr };
        controls::Grid shortcutRow{ nullptr };
        HWND owner{};
        HWND hwnd{};
        uint32_t appearanceMode{ kAppearanceSystem };
        uint32_t languageId{ kLanguageEnglish };
        uint32_t shortcutModifiers{ MOD_CONTROL | MOD_ALT };
        uint32_t shortcutKey{ 'S' };
        bool startupLastKnownEnabled{};
        bool startupHasKnownValue{};
        bool startupLoading{ true };
        bool appearanceLoading{ true };
        bool languageLoading{ true };
        bool shortcutRecording{};
        bool customTitleBarActive{};
        bool alive{ true };
    };

    bool FindFirstButton(
        x::DependencyObject const& parent,
        controls::Button& result)
    {
        const auto count = media::VisualTreeHelper::GetChildrenCount(parent);
        for (int32_t index = 0; index < count; ++index)
        {
            auto child = media::VisualTreeHelper::GetChild(parent, index);
            if (auto button = child.try_as<controls::Button>())
            {
                result = button;
                return true;
            }
            if (FindFirstButton(child, result)) return true;
        }
        return false;
    }

    void UpdateTitleBarDragRegions(std::shared_ptr<SettingsState> const& state) noexcept
    {
        if (!state || !state->alive || !state->customTitleBarActive ||
            !state->appWindowTitleBar || !state->titleBar || !state->shellRoot || !state->hwnd)
            return;

        try
        {
            RECT client{};
            if (!::GetClientRect(state->hwnd, &client)) return;
            const double scale = state->titleBar.XamlRoot().RasterizationScale();
            if (scale <= 0.0) return;

            const auto rowTopDip = state->titleBar.TransformToVisual(state->shellRoot)
                .TransformPoint({ 0.0, 0.0 }).Y;
            const auto top = static_cast<int32_t>(std::max(0.0, std::round(rowTopDip * scale)));
            const auto bottom = static_cast<int32_t>(std::min(
                static_cast<double>(client.bottom),
                std::round((rowTopDip + state->titleBar.ActualHeight()) * scale)));
            const auto right = std::max<int32_t>(0,
                (client.right - client.left) - state->appWindowTitleBar.RightInset());
            if (bottom <= top || right <= 0) return;

            std::vector<winrt::Windows::Graphics::RectInt32> dragRegions;
            controls::Button backButton{ nullptr };
            if (FindFirstButton(state->titleBar, backButton) &&
                backButton.Visibility() == x::Visibility::Visible && backButton.IsEnabled())
            {
                const auto origin = backButton.TransformToVisual(state->shellRoot)
                    .TransformPoint({ 0.0, 0.0 });
                const auto leftOfButton = static_cast<int32_t>(std::max(
                    0.0, std::round(origin.X * scale)));
                const auto rightOfButton = static_cast<int32_t>(std::min(
                    static_cast<double>(right),
                    std::round((origin.X + backButton.ActualWidth()) * scale)));
                if (leftOfButton > 0)
                    dragRegions.push_back({ 0, top, leftOfButton, bottom - top });
                if (rightOfButton < right)
                    dragRegions.push_back({ rightOfButton, top,
                        right - rightOfButton, bottom - top });
            }
            else
            {
                dragRegions.push_back({ 0, top, right, bottom - top });
            }
            state->appWindowTitleBar.SetDragRectangles(dragRegions);
        }
        catch (...)
        {
            // Layout can be transient while the XAML island is measuring or changing DPI.
        }
    }

    void EndShortcutCaptureNoThrow(std::shared_ptr<SettingsState> const& state) noexcept
    {
        if (!state || !state->shortcutRecording) return;
        try
        {
            state->shortcutRecording = false;
            if (state->shortcutButton)
                state->shortcutButton.Content(winrt::box_value(FormatShortcut(
                    state->shortcutModifiers, state->shortcutKey)));
            if (state->owner && ::IsWindow(state->owner))
                ::SendMessageW(state->owner, WM_GLOBAL_SHORTCUT_CAPTURE, FALSE, 0);
        }
        catch (...)
        {
            // Closing a WinUI island must not strand the app's hotkey disabled.
            if (state->owner && ::IsWindow(state->owner))
                ::SendMessageW(state->owner, WM_GLOBAL_SHORTCUT_CAPTURE, FALSE, 0);
        }
    }

    void UpdateSettingsCardWidths(
        std::shared_ptr<SettingsState> const& state,
        double fullWidthDips)
    {
        if (!state || !state->navigation || !state->startupCard ||
            !state->appearanceCard || !state->languageCard || !state->shortcutCard ||
            !state->startupRow || !state->appearanceRow || !state->languageRow ||
            !state->shortcutRow)
            return;
        // LeftCompact opens as an overlay, so the content viewport stays the
        // same width. Calculate from the persistent compact rail only.
        const double paneWidth = state->navigation.CompactPaneLength();
        const double cardWidth = fullWidthDips - paneWidth -
            (2.0 * kSettingsContentGutterDips);
        if (cardWidth <= 28.0) return;
        state->startupCard.Width(cardWidth);
        state->appearanceCard.Width(cardWidth);
        state->languageCard.Width(cardWidth);
        state->shortcutCard.Width(cardWidth);
        const double rowWidth = cardWidth - 32.0;
        state->startupRow.Width(rowWidth);
        state->appearanceRow.Width(rowWidth);
        state->languageRow.Width(rowWidth);
        state->shortcutRow.Width(rowWidth);
    }

    void RefreshHistoryNoThrow(std::shared_ptr<SettingsState> const& state) noexcept
    {
        if (!state || !state->alive || !state->historyItems || !state->historyEmpty)
            return;
        try
        {
            state->historyItems.Children().Clear();
            const auto count = state->historySnapshot.size();
            state->historyEmpty.Visibility(count == 0
                ? x::Visibility::Visible : x::Visibility::Collapsed);

            for (size_t index = 0; index < count; ++index)
            {
                auto card = markup::XamlReader::Load(
                    LR"(<Border xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation" Background="{ThemeResource CardBackgroundFillColorDefaultBrush}" CornerRadius="8" Padding="16" />)"
                ).as<controls::Border>();
                card.HorizontalAlignment(x::HorizontalAlignment::Stretch);

                controls::Grid row;
                row.ColumnSpacing(12.0);
                auto iconColumn = controls::ColumnDefinition{};
                iconColumn.Width(x::GridLengthHelper::FromValueAndType(24.0, x::GridUnitType::Pixel));
                auto textColumn = controls::ColumnDefinition{};
                textColumn.Width(x::GridLengthHelper::FromValueAndType(1.0, x::GridUnitType::Star));
                auto linkColumn = controls::ColumnDefinition{};
                linkColumn.Width(x::GridLengthHelper::FromValueAndType(32.0, x::GridUnitType::Pixel));
                row.ColumnDefinitions().Append(iconColumn);
                row.ColumnDefinitions().Append(textColumn);
                row.ColumnDefinitions().Append(linkColumn);

                controls::FontIcon musicIcon;
                musicIcon.Glyph(L"\uE189"); // MusicNote
                musicIcon.FontSize(18.0);
                musicIcon.VerticalAlignment(x::VerticalAlignment::Center);
                controls::Grid::SetColumn(musicIcon, 0);
                row.Children().Append(musicIcon);

                controls::StackPanel text;
                text.Spacing(3.0);
                text.VerticalAlignment(x::VerticalAlignment::Center);
                controls::TextBlock title;
                title.Text(state->historySnapshot[index].title);
                title.FontSize(16.0);
                title.FontWeight(winrt::Windows::UI::Text::FontWeights::SemiBold());
                title.TextWrapping(x::TextWrapping::NoWrap);
                title.TextTrimming(x::TextTrimming::CharacterEllipsis);
                automation::AutomationProperties::SetName(title, title.Text());
                text.Children().Append(title);

                controls::TextBlock artist;
                artist.Text(state->historySnapshot[index].artist);
                artist.FontSize(14.0);
                artist.TextWrapping(x::TextWrapping::NoWrap);
                artist.TextTrimming(x::TextTrimming::CharacterEllipsis);
                automation::AutomationProperties::SetName(artist, artist.Text());
                text.Children().Append(artist);

                controls::TextBlock recognizedAt;
                recognizedAt.Text(state->historySnapshot[index].recognizedAt);
                recognizedAt.FontSize(12.0);
                recognizedAt.Opacity(0.72);
                recognizedAt.TextWrapping(x::TextWrapping::NoWrap);
                recognizedAt.TextTrimming(x::TextTrimming::CharacterEllipsis);
                text.Children().Append(recognizedAt);

                controls::Grid::SetColumn(text, 1);
                row.Children().Append(text);

                auto const& pageUrl = state->historySnapshot[index].pageUrl;
                if (!pageUrl.empty())
                {
                    controls::FontIcon openIcon;
                    openIcon.Glyph(L"\uE8A7"); // OpenInNewWindow
                    openIcon.FontSize(16.0);
                    openIcon.HorizontalAlignment(x::HorizontalAlignment::Right);
                    openIcon.VerticalAlignment(x::VerticalAlignment::Center);
                    controls::Grid::SetColumn(openIcon, 2);
                    row.Children().Append(openIcon);
                }

                card.Child(row);
                if (!pageUrl.empty())
                {
                    controls::Button openPage;
                    openPage.Padding(x::ThicknessHelper::FromUniformLength(0.0));
                    openPage.BorderThickness(x::ThicknessHelper::FromUniformLength(0.0));
                    openPage.Background(media::SolidColorBrush(
                        winrt::Microsoft::UI::ColorHelper::FromArgb(0, 0, 0, 0)));
                    openPage.HorizontalAlignment(x::HorizontalAlignment::Stretch);
                    openPage.HorizontalContentAlignment(x::HorizontalAlignment::Stretch);
                    openPage.Content(card);
                    automation::AutomationProperties::SetName(openPage, Localized(
                        L"Open Shazam page", L"Shazamページを開く"));
                    automation::AutomationProperties::SetHelpText(
                        openPage, winrt::hstring(state->historySnapshot[index].title));
                    openPage.Click([pageUrl](auto const&, auto const&) {
                        ::ShellExecuteW(nullptr, L"open", pageUrl.c_str(), nullptr, nullptr,
                            SW_SHOWNORMAL);
                    });
                    state->historyItems.Children().Append(openPage);
                }
                else
                {
                    state->historyItems.Children().Append(card);
                }

            }
        }
        catch (...)
        {
            // Keep the Settings host responsive even if a history row cannot
            // be materialized while the WinUI island is changing state.
        }
    }

    void SetHistoryNoThrow(
        std::shared_ptr<SettingsState> const& state,
        OpenShazamHistoryItem const* items,
        size_t count) noexcept
    {
        if (!state || !state->alive) return;
        try
        {
            state->historySnapshot.clear();
            if (!items) count = 0;
            count = (std::min)(count, size_t{100});
            state->historySnapshot.reserve(count);
            for (size_t index = 0; index < count; ++index)
            {
                state->historySnapshot.push_back({
                    items[index].title ? items[index].title : L"",
                    items[index].artist ? items[index].artist : L"",
                    items[index].recognizedAt ? items[index].recognizedAt : L"",
                    items[index].pageUrl ? items[index].pageUrl : L"",
                });
            }
            if (state->hwnd && ::IsWindowVisible(state->hwnd) &&
                state->historyViewer.Visibility() == x::Visibility::Visible)
                RefreshHistoryNoThrow(state);
        }
        catch (...)
        {
            // A failed snapshot refresh should not affect recognition state.
        }
    }

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

    void UpdateStartupValueNoThrow(std::shared_ptr<SettingsState> const& state) noexcept
    {
        if (!state || !state->alive) return;
        try
        {
            state->startupValue.Text(state->toggle.IsOn()
                ? Localized(L"On", L"オン")
                : Localized(L"Off", L"オフ"));
            state->startupValue.Opacity(state->toggle.IsEnabled() ? 1.0 : 0.5);
        }
        catch (...)
        {
            // WinRT property updates can fail while the island is closing.
        }
    }

    void ApplyLanguageToSettingsNoThrow(std::shared_ptr<SettingsState> const& state) noexcept
    {
        if (!state || !state->alive) return;
        try
        {
            const auto settingsText = Localized(L"Settings", L"\u8A2D\u5B9A");
            if (state->settingsPageTitle)
            {
                state->settingsPageTitle.Text(settingsText);
                automation::AutomationProperties::SetName(state->settingsPageTitle, settingsText);
            }
            if (state->startupTitle)
            {
                const auto text = Localized(L"Open at startup", L"\u8D77\u52D5\u6642\u306B\u958B\u304F");
                state->startupTitle.Text(text);
                automation::AutomationProperties::SetName(state->startupTitle, text);
            }
            if (state->startupDescription)
            {
                const auto text = Localized(
                    L"Start Shazam every time Windows starts",
                    L"Windows\u3092\u8D77\u52D5\u3059\u308B\u305F\u3073\u306BShazam\u30A2\u30D7\u30EA\u3082\u8D77\u52D5");
                state->startupDescription.Text(text);
                automation::AutomationProperties::SetName(state->startupDescription, text);
            }
            if (state->appearanceTitle)
            {
                const auto text = Localized(L"Theme", L"\u30C6\u30FC\u30DE");
                state->appearanceTitle.Text(text);
                automation::AutomationProperties::SetName(state->appearanceTitle, text);
            }
            if (state->appearanceDescription)
            {
                const auto text = Localized(
                    L"Choose the app theme to display",
                    L"\u8868\u793A\u3059\u308B\u30A2\u30D7\u30EA\u306E\u30C6\u30FC\u30DE\u3092\u9078\u629E\u3057\u307E\u3059");
                state->appearanceDescription.Text(text);
                automation::AutomationProperties::SetName(state->appearanceDescription, text);
            }
            if (state->languageTitle)
            {
                const auto text = Localized(L"Language", L"\u8A00\u8A9E");
                state->languageTitle.Text(text);
                automation::AutomationProperties::SetName(state->languageTitle, text);
                if (state->language)
                    automation::AutomationProperties::SetName(state->language, text);
            }
            if (state->languageDescription)
            {
                const auto text = Localized(
                    L"Choose the display language.",
                    L"\u8868\u793A\u8A00\u8A9E\u3092\u9078\u629E\u3057\u307E\u3059");
                state->languageDescription.Text(text);
                automation::AutomationProperties::SetName(state->languageDescription, text);
            }
            if (state->shortcutTitle)
            {
                const auto text = Localized(L"Shortcut", L"\u30B7\u30E7\u30FC\u30C8\u30AB\u30C3\u30C8");
                state->shortcutTitle.Text(text);
                automation::AutomationProperties::SetName(state->shortcutTitle, text);
            }
            if (state->shortcutDescription)
            {
                const auto text = Localized(
                    L"Enter the keyboard shortcut to start Shazam from anywhere in Windows.",
                    L"Windows\u4E0A\u306E\u3069\u3053\u304B\u3089\u3067\u3082 Shazam \u3092\u958B\u59CB\u3057\u305F\u3044\u3068\u304D\u306B\u4F7F\u3046\u30AD\u30FC\u30DC\u30FC\u30C9\u30B7\u30E7\u30FC\u30C8\u30AB\u30C3\u30C8\u3092\u5165\u529B\u3057\u307E\u3057\u3087\u3046\u3002");
                state->shortcutDescription.Text(text);
                automation::AutomationProperties::SetName(state->shortcutDescription, text);
            }

            if (state->appearance)
            {
                state->appearanceLoading = true;
                state->appearance.Items().Clear();
                state->appearance.Items().Append(winrt::box_value(Localized(L"Light", L"\u30E9\u30A4\u30C8")));
                state->appearance.Items().Append(winrt::box_value(Localized(L"Dark", L"\u30C0\u30FC\u30AF")));
                state->appearance.Items().Append(winrt::box_value(Localized(L"System", L"\u30B7\u30B9\u30C6\u30E0")));
                state->appearance.SelectedIndex(ComboIndexFromAppearance(state->appearanceMode));
                state->appearanceLoading = false;
            }
            if (state->language)
            {
                state->languageLoading = true;
                state->language.Items().Clear();
                state->language.Items().Append(winrt::box_value(Localized(L"Japanese", L"\u65E5\u672C\u8A9E")));
                state->language.Items().Append(winrt::box_value(Localized(L"English", L"\u82F1\u8A9E")));
                state->language.SelectedIndex(ComboIndexFromLanguage(state->languageId));
                state->languageLoading = false;
            }

            if (state->historyTitle)
            {
                const auto text = Localized(L"History", L"\u5C65\u6B74");
                state->historyTitle.Text(text);
                automation::AutomationProperties::SetName(state->historyTitle, text);
                if (state->historyNavigationItem)
                {
                    state->historyNavigationItem.Content(winrt::box_value(text));
                    automation::AutomationProperties::SetName(state->historyNavigationItem, text);
                }
            }
            if (state->historyEmpty)
            {
                const auto text = Localized(
                    L"No recognized songs",
                    L"\u307E\u3060\u8A8D\u8B58\u3057\u305F\u66F2\u306F\u3042\u308A\u307E\u305B\u3093");
                state->historyEmpty.Text(text);
                automation::AutomationProperties::SetName(state->historyEmpty, text);
            }
            if (state->navigation)
            {
                if (auto settingsItem = state->navigation.SettingsItem()
                        .try_as<controls::NavigationViewItem>())
                {
                    settingsItem.Content(winrt::box_value(settingsText));
                    automation::AutomationProperties::SetName(settingsItem, settingsText);
                }
            }
            UpdateStartupValueNoThrow(state);
            if (state->shortcutButton && !state->shortcutRecording)
                state->shortcutButton.Content(winrt::box_value(FormatShortcut(
                    state->shortcutModifiers, state->shortcutKey)));
            if (state->shortcutInfoBar && state->shortcutInfoBar.IsOpen())
            {
                state->shortcutInfoBar.Title(Localized(
                    L"Shortcut is unavailable", L"\u3053\u306E\u30B7\u30E7\u30FC\u30C8\u30AB\u30C3\u30C8\u306F\u4F7F\u7528\u3067\u304D\u307E\u305B\u3093"));
                state->shortcutInfoBar.Message(Localized(
                    L"Choose a different key combination.",
                    L"\u5225\u306E\u30AD\u30FC\u306E\u7D44\u307F\u5408\u308F\u305B\u3092\u6307\u5B9A\u3057\u3066\u304F\u3060\u3055\u3044\u3002"));
            }
        }
        catch (...)
        {
            state->appearanceLoading = false;
            state->languageLoading = false;
        }
    }

    void ApplyAppearanceToSettings(std::shared_ptr<SettingsState> const& state, uint32_t mode)
    {
        if (!state || !state->alive) return;
        if (mode > kAppearanceDark) mode = kAppearanceSystem;
        state->appearanceMode = mode;
        if (state->shellRoot)
        {
            auto theme = mode == kAppearanceDark ? x::ElementTheme::Dark :
                mode == kAppearanceLight ? x::ElementTheme::Light : x::ElementTheme::Default;
            state->shellRoot.RequestedTheme(theme);
        }
        if (state->customTitleBarActive && state->appWindowTitleBar)
        {
            try
            {
                const auto titleBarTheme = mode == kAppearanceSystem
                    ? winrt::Microsoft::UI::Windowing::TitleBarTheme::UseDefaultAppMode
                    : (mode == kAppearanceDark
                        ? winrt::Microsoft::UI::Windowing::TitleBarTheme::Dark
                        : winrt::Microsoft::UI::Windowing::TitleBarTheme::Light);
                state->appWindowTitleBar.PreferredTheme(titleBarTheme);
            }
            catch (...)
            {
                // Older Windows versions retain their native title-bar theme.
            }
        }
    }

    void ShowStartupInfoNoThrow(
        std::shared_ptr<SettingsState> const& state,
        controls::InfoBarSeverity severity,
        winrt::hstring const& title,
        winrt::hstring const& message) noexcept
    {
        if (!state || !state->alive) return;
        try
        {
            if (!state->startupInfoBar) return;
            state->startupInfoBar.Severity(severity);
            state->startupInfoBar.Title(title);
            state->startupInfoBar.Message(message);
            state->startupInfoBar.Visibility(x::Visibility::Visible);
            state->startupInfoBar.IsOpen(true);
        }
        catch (...)
        {
            // Keep native error handling from escaping the WinUI event loop.
        }
    }

    void ApplyStartupStateNoThrow(
        std::shared_ptr<SettingsState> const& state,
        bool enabled,
        bool canChange) noexcept
    {
        if (!state || !state->alive) return;
        try
        {
            state->startupLastKnownEnabled = enabled;
            state->startupHasKnownValue = true;
            state->startupValue.Visibility(x::Visibility::Visible);
            state->toggle.Visibility(x::Visibility::Visible);
            state->toggle.IsOn(enabled);
            state->toggle.IsEnabled(canChange);
            state->startupLoading = false;
            if (state->startupInfoBar)
            {
                state->startupInfoBar.IsOpen(false);
                state->startupInfoBar.Visibility(x::Visibility::Collapsed);
            }
            UpdateStartupValueNoThrow(state);
        }
        catch (...)
        {
            // Keep native error handling from escaping the WinUI event loop.
        }
    }

    void ReportStartupFailureNoThrow(
        std::shared_ptr<SettingsState> const& state,
        bool changeFailed) noexcept
    {
        if (!state || !state->alive) return;
        state->startupLoading = false;
        try
        {
            if (state->startupHasKnownValue)
            {
                state->toggle.IsOn(state->startupLastKnownEnabled);
                state->startupValue.Visibility(x::Visibility::Visible);
                state->toggle.Visibility(x::Visibility::Visible);
            }
            else
            {
                state->startupValue.Visibility(x::Visibility::Collapsed);
                state->toggle.Visibility(x::Visibility::Collapsed);
            }
            state->toggle.IsEnabled(false);
            UpdateStartupValueNoThrow(state);
        }
        catch (...)
        {
            // The InfoBar below is still best-effort while the island is alive.
        }

        ShowStartupInfoNoThrow(
            state,
            controls::InfoBarSeverity::Error,
            changeFailed
                ? Localized(L"Could not update start on sign-in", L"自動起動を更新できませんでした")
                : Localized(L"Could not read start on sign-in", L"自動起動の状態を取得できませんでした"),
            changeFailed
                ? Localized(
                    L"The last known state is shown. Check Windows startup settings to confirm whether the change was applied.",
                    L"以前に確認できた状態を表示しています。変更が反映されたか Windows のスタートアップ設定で確認してください。")
                : Localized(
                    L"Windows did not return the current state. Reopen Settings and try again.",
                    L"Windows から現在の状態を取得できませんでした。設定を開き直してください。"));
    }

    void ReportStartupDisabledByWindowsNoThrow(
        std::shared_ptr<SettingsState> const& state) noexcept
    {
        if (!state || !state->alive) return;
        ApplyStartupStateNoThrow(state, false, false);
        ShowStartupInfoNoThrow(
            state,
            controls::InfoBarSeverity::Informational,
            Localized(L"Start on sign-in is off in Windows", L"Windows \u5074\u3067\u81ea\u52d5\u8d77\u52d5\u304c\u30aa\u30d5\u3067\u3059"),
            Localized(
                L"Windows has disabled startup for Shazam. Change it in Windows Startup settings.",
                L"Windows \u5074\u3067 Shazam \u306e\u81ea\u52d5\u8d77\u52d5\u304c\u7121\u52b9\u3067\u3059\u3002Windows \u306e\u30b9\u30bf\u30fc\u30c8\u30a2\u30c3\u30d7\u8a2d\u5b9a\u304b\u3089\u5909\u66f4\u3057\u3066\u304f\u3060\u3055\u3044."));

    }

    void RefreshStartup(std::weak_ptr<SettingsState> weak)
    {
        auto state = weak.lock();
        if (!state || !state->alive) return;
        try
        {
            state->startupLoading = true;
            state->toggle.IsEnabled(false);
            const auto current = ReadStartupStatus();
            if (current.disabledByWindows)
                ReportStartupDisabledByWindowsNoThrow(state);
            else
                ApplyStartupStateNoThrow(state, current.enabled, current.canChange);
        }
        catch (...)
        {
            ReportStartupFailureNoThrow(state, false);
        }
    }

    void ChangeStartup(std::weak_ptr<SettingsState> weak, bool enable)
    {
        auto state = weak.lock();
        if (!state || !state->alive) return;
        try
        {
            state->startupLoading = true;
            state->toggle.IsEnabled(false);
            SetRunStartupEnabled(enable);
            if (!state->alive) return;
            const auto current = ReadStartupStatus();
            if (current.disabledByWindows)
                ReportStartupDisabledByWindowsNoThrow(state);
            else
                ApplyStartupStateNoThrow(state, current.enabled, current.canChange);
        }
        catch (...)
        {
            ReportStartupFailureNoThrow(state, true);
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

        void Initialize(HWND parent, uint32_t languageId)
        {
            g_uiLanguage = languageId == kLanguageJapanese
                ? kLanguageJapanese : kLanguageEnglish;
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
            wc.hIcon = ::LoadIconW(instance, MAKEINTRESOURCEW(101));
            wc.lpszClassName = kSettingsClass;
            wc.hCursor = ::LoadCursorW(nullptr, IDC_ARROW);
            // The XAML island paints the entire client area. Avoid the native
            // COLOR_WINDOW erase, which flashes white and conflicts with the
            // WinUI theme surface during startup and theme changes.
            wc.hbrBackground = static_cast<HBRUSH>(::GetStockObject(NULL_BRUSH));
            if (!::RegisterClassW(&wc) && ::GetLastError() != ERROR_CLASS_ALREADY_EXISTS)
                winrt::throw_last_error();

            auto dpi = ::GetDpiForWindow(parent);
            if (dpi < 96) dpi = 96;
            auto windowTitle = Localized(L"Shazam", L"Shazam");
            hwnd = ::CreateWindowExW(0, kSettingsClass, windowTitle.c_str(),
                WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN, CW_USEDEFAULT, CW_USEDEFAULT,
                ::MulDiv(kWidthDips, dpi, 96), ::MulDiv(kHeightDips, dpi, 96),
                parent, nullptr, instance, nullptr);
            if (!hwnd) winrt::throw_last_error();
            state = std::make_shared<SettingsState>();
            state->owner = owner;
            state->hwnd = hwnd;
            state->languageId = g_uiLanguage;
            try
            {
                auto appWindow = winrt::Microsoft::UI::Windowing::AppWindow::GetFromWindowId(
                    winrt::Microsoft::UI::GetWindowIdFromWindow(hwnd));
                if (appWindow &&
                    winrt::Microsoft::UI::Windowing::AppWindowTitleBar::IsCustomizationSupported())
                {
                    state->appWindowTitleBar = appWindow.TitleBar();
                    state->appWindowTitleBar.ExtendsContentIntoTitleBar(true);
                    state->appWindowTitleBar.PreferredHeightOption(
                        winrt::Microsoft::UI::Windowing::TitleBarHeightOption::Tall);
                    const auto transparentCaptionBackground = winrt::box_value(
                        winrt::Windows::UI::Color{ 0, 0, 0, 0 })
                        .as<winrt::Windows::Foundation::IReference<winrt::Windows::UI::Color>>();
                    state->appWindowTitleBar.ButtonBackgroundColor(transparentCaptionBackground);
                    state->appWindowTitleBar.ButtonInactiveBackgroundColor(transparentCaptionBackground);
                    state->customTitleBarActive = true;
                }
            }
            catch (...)
            {
                // Older Windows versions keep the standard non-client title bar.
            }
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
            island.Content(BuildContent());
            Layout();
            RefreshStartup(state);
        }

        x::UIElement BuildContent()
        {
            state->settingsContent = controls::StackPanel{};
            auto stack = state->settingsContent;
            stack.Spacing(kSettingsSectionSpacingDips);
            stack.Padding(x::ThicknessHelper::FromLengths(
                kSettingsContentGutterDips, 24.0, kSettingsContentGutterDips, 24.0));

            controls::TextBlock pageTitle;
            pageTitle.Text(Localized(L"Settings", L"\u8A2D\u5B9A"));
            pageTitle.FontSize(28.0);
            pageTitle.FontWeight(winrt::Windows::UI::Text::FontWeights::Bold());
            pageTitle.HorizontalAlignment(x::HorizontalAlignment::Left);
            pageTitle.Margin(x::ThicknessHelper::FromLengths(0.0, 0.0, 0.0, 0.0));
            automation::AutomationProperties::SetName(
                pageTitle, Localized(L"Settings", L"\u8A2D\u5B9A"));
            state->settingsPageTitle = pageTitle;
            stack.Children().Append(pageTitle);

            state->startupCard = markup::XamlReader::Load(
                LR"(<Border xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation" Background="{ThemeResource CardBackgroundFillColorDefaultBrush}" CornerRadius="8" />)"
            ).as<controls::Border>();
            state->startupCard.Padding(
                x::ThicknessHelper::FromUniformLength(kSettingsCardPaddingDips));
            state->startupCard.HorizontalAlignment(x::HorizontalAlignment::Stretch);
            state->startupRow = controls::Grid{};
            auto startupRow = state->startupRow;
            startupRow.HorizontalAlignment(x::HorizontalAlignment::Stretch);
            startupRow.ColumnSpacing(12.0);
            auto startupIconColumn = controls::ColumnDefinition{};
            startupIconColumn.Width(x::GridLengthHelper::FromValueAndType(34.0, x::GridUnitType::Pixel));
            auto startupLabelColumn = controls::ColumnDefinition{};
            startupLabelColumn.Width(x::GridLengthHelper::FromValueAndType(1.0, x::GridUnitType::Star));
            auto startupToggleColumn = controls::ColumnDefinition{};
            // Share the same trailing lane width as the appearance selector
            // so both settings controls align to one right edge.
            startupToggleColumn.Width(
                x::GridLengthHelper::FromValueAndType(
                    kSettingsControlLaneWidthDips, x::GridUnitType::Pixel));
            startupRow.ColumnDefinitions().Append(startupIconColumn);
            startupRow.ColumnDefinitions().Append(startupLabelColumn);
            startupRow.ColumnDefinitions().Append(startupToggleColumn);

            controls::FontIcon startupIcon;
            startupIcon.Glyph(L"\uE7E8"); // PowerButton
            startupIcon.FontSize(20.0);
            startupIcon.HorizontalAlignment(x::HorizontalAlignment::Left);
            startupIcon.VerticalAlignment(x::VerticalAlignment::Center);
            controls::Grid::SetColumn(startupIcon, 0);
            startupRow.Children().Append(startupIcon);

            controls::StackPanel startupLabels;
            startupLabels.Spacing(2.0);
            startupLabels.HorizontalAlignment(x::HorizontalAlignment::Left);
            startupLabels.VerticalAlignment(x::VerticalAlignment::Center);
            controls::TextBlock startupLabel;
            state->startupTitle = startupLabel;
            state->toggle = controls::ToggleSwitch{};
            auto startupName = Localized(L"Open at startup", L"起動時に開く");
            startupLabel.Text(startupName);
            startupLabel.FontSize(14.0);
            startupLabel.HorizontalAlignment(x::HorizontalAlignment::Left);
            startupLabel.VerticalAlignment(x::VerticalAlignment::Center);
            startupLabel.TextAlignment(x::TextAlignment::Left);
            automation::AutomationProperties::SetName(startupLabel, startupName);
            startupLabels.Children().Append(startupLabel);
            controls::TextBlock startupDescription;
            state->startupDescription = startupDescription;
            auto startupDescriptionText = Localized(
                L"Start Shazam every time Windows starts",
                L"Windowsを起動するたびにShazamアプリも起動");
            startupDescription.Text(startupDescriptionText);
            startupDescription.FontSize(12.0);
            startupDescription.TextWrapping(x::TextWrapping::Wrap);
            startupDescription.HorizontalAlignment(x::HorizontalAlignment::Left);
            startupDescription.TextAlignment(x::TextAlignment::Left);
            automation::AutomationProperties::SetName(startupDescription, startupDescriptionText);
            startupLabels.Children().Append(startupDescription);
            controls::Grid::SetColumn(startupLabels, 1);
            startupRow.Children().Append(startupLabels);

            automation::AutomationProperties::SetName(state->toggle, startupName);
            state->startupValue = controls::TextBlock{};
            state->startupValue.Text(Localized(L"Off", L"オフ"));
            state->startupValue.FontSize(14.0);
            state->startupValue.Width(38.0);
            state->startupValue.TextAlignment(x::TextAlignment::Center);
            state->startupValue.VerticalAlignment(x::VerticalAlignment::Center);
            state->toggle.HorizontalAlignment(x::HorizontalAlignment::Left);
            state->toggle.VerticalAlignment(x::VerticalAlignment::Center);
            state->toggle.OnContent(winrt::box_value(Localized(L"", L"")));
            state->toggle.OffContent(winrt::box_value(Localized(L"", L"")));
            state->toggle.Width(48.0);
            state->toggle.IsEnabled(false);

            controls::StackPanel startupControl;
            startupControl.Orientation(controls::Orientation::Horizontal);
            startupControl.Spacing(8.0);
            // The 38-DIP status label, 8-DIP gap and 48-DIP switch total
            // 94 DIPs. Reserve the remaining leading space in the shared
            // 208-DIP control lane so this compact group shares the ComboBox
            // trailing edge; the Windows UIA smoke test checks the result.
            startupControl.Width(kStartupControlGroupWidthDips);
            startupControl.Margin(x::ThicknessHelper::FromLengths(
                kSettingsControlLaneWidthDips - kStartupControlGroupWidthDips,
                0.0, 0.0, 0.0));
            startupControl.HorizontalAlignment(x::HorizontalAlignment::Left);
            startupControl.VerticalAlignment(x::VerticalAlignment::Center);
            startupControl.Children().Append(state->startupValue);
            startupControl.Children().Append(state->toggle);

            std::weak_ptr<SettingsState> weak = state;
            state->toggle.Toggled([weak](auto const&, auto const&) {
                if (auto state = weak.lock(); state && state->alive)
                {
                    UpdateStartupValueNoThrow(state);
                    if (!state->startupLoading) ChangeStartup(weak, state->toggle.IsOn());
                }
            });
            controls::Grid::SetColumn(startupControl, 2);
            startupRow.Children().Append(startupControl);
            state->startupCard.Child(startupRow);

            // Normal settings stay compact. This standard WinUI InfoBar opens
            // only when Windows denies or cannot confirm the startup state.
            state->startupInfoBar = controls::InfoBar{};
            state->startupInfoBar.IsClosable(false);
            state->startupInfoBar.IsOpen(false);
            state->startupInfoBar.Visibility(x::Visibility::Collapsed);
            state->startupInfoBar.Margin(x::ThicknessHelper::FromLengths(
                0.0, kSettingsInlineMessageSpacingDips, 0.0, 0.0));
            state->startupInfoBar.Severity(controls::InfoBarSeverity::Informational);
            controls::StackPanel startupSection;
            startupSection.Spacing(0.0);
            startupSection.Children().Append(state->startupCard);
            startupSection.Children().Append(state->startupInfoBar);
            stack.Children().Append(startupSection);

            state->appearanceCard = markup::XamlReader::Load(
                LR"(<Border xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation" Background="{ThemeResource CardBackgroundFillColorDefaultBrush}" CornerRadius="8" />)"
            ).as<controls::Border>();
            state->appearanceCard.Padding(
                x::ThicknessHelper::FromUniformLength(kSettingsCardPaddingDips));
            state->appearanceCard.HorizontalAlignment(x::HorizontalAlignment::Stretch);
            state->appearanceRow = controls::Grid{};
            auto appearanceRow = state->appearanceRow;
            appearanceRow.HorizontalAlignment(x::HorizontalAlignment::Stretch);
            appearanceRow.ColumnSpacing(12.0);
            auto iconColumn = controls::ColumnDefinition{};
            iconColumn.Width(x::GridLengthHelper::FromValueAndType(34.0, x::GridUnitType::Pixel));
            auto labelColumn = controls::ColumnDefinition{};
            labelColumn.Width(x::GridLengthHelper::FromValueAndType(1.0, x::GridUnitType::Star));
            auto selectorColumn = controls::ColumnDefinition{};
            selectorColumn.Width(x::GridLengthHelper::FromValueAndType(
                kAppearanceSelectorWidthDips, x::GridUnitType::Pixel));
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
            state->appearanceTitle = appearanceTitle;
            auto appearanceName = Localized(L"Theme", L"テーマ");
            appearanceTitle.Text(appearanceName);
            appearanceTitle.FontSize(14.0);
            appearanceTitle.HorizontalAlignment(x::HorizontalAlignment::Left);
            appearanceTitle.TextAlignment(x::TextAlignment::Left);
            automation::AutomationProperties::SetName(appearanceTitle, appearanceName);
            controls::TextBlock appearanceDescription;
            state->appearanceDescription = appearanceDescription;
            auto appearanceDescriptionText = Localized(
                L"Choose the app theme to display",
                L"表示するアプリのテーマを選択します");
            appearanceDescription.Text(appearanceDescriptionText);
            appearanceDescription.FontSize(12.0);
            appearanceDescription.TextWrapping(x::TextWrapping::Wrap);
            appearanceDescription.HorizontalAlignment(x::HorizontalAlignment::Left);
            appearanceDescription.TextAlignment(x::TextAlignment::Left);
            automation::AutomationProperties::SetName(appearanceDescription, appearanceDescriptionText);
            appearanceLabels.Children().Append(appearanceTitle);
            appearanceLabels.Children().Append(appearanceDescription);
            controls::Grid::SetColumn(appearanceLabels, 1);
            appearanceRow.Children().Append(appearanceLabels);

            state->appearance = controls::ComboBox{};
            state->appearance.Items().Append(winrt::box_value(Localized(L"Light", L"ライト")));
            state->appearance.Items().Append(winrt::box_value(Localized(L"Dark", L"ダーク")));
            state->appearance.Items().Append(winrt::box_value(Localized(
                L"System", L"システム")));
            state->appearance.Width(kAppearanceSelectorWidthDips);
            state->appearance.MinWidth(kAppearanceSelectorWidthDips);
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

            state->languageCard = markup::XamlReader::Load(
                LR"(<Border xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation" Background="{ThemeResource CardBackgroundFillColorDefaultBrush}" CornerRadius="8" />)"
            ).as<controls::Border>();
            state->languageCard.Padding(
                x::ThicknessHelper::FromUniformLength(kSettingsCardPaddingDips));
            state->languageCard.HorizontalAlignment(x::HorizontalAlignment::Stretch);
            state->languageRow = controls::Grid{};
            auto languageRow = state->languageRow;
            languageRow.HorizontalAlignment(x::HorizontalAlignment::Stretch);
            languageRow.ColumnSpacing(12.0);
            auto languageIconColumn = controls::ColumnDefinition{};
            languageIconColumn.Width(x::GridLengthHelper::FromValueAndType(34.0, x::GridUnitType::Pixel));
            auto languageLabelColumn = controls::ColumnDefinition{};
            languageLabelColumn.Width(x::GridLengthHelper::FromValueAndType(1.0, x::GridUnitType::Star));
            auto languageSelectorColumn = controls::ColumnDefinition{};
            languageSelectorColumn.Width(x::GridLengthHelper::FromValueAndType(
                kAppearanceSelectorWidthDips, x::GridUnitType::Pixel));
            languageRow.ColumnDefinitions().Append(languageIconColumn);
            languageRow.ColumnDefinitions().Append(languageLabelColumn);
            languageRow.ColumnDefinitions().Append(languageSelectorColumn);

            controls::FontIcon languageIcon;
            languageIcon.Glyph(L"\uE774"); // Globe
            languageIcon.FontSize(20.0);
            languageIcon.HorizontalAlignment(x::HorizontalAlignment::Left);
            languageIcon.VerticalAlignment(x::VerticalAlignment::Center);
            controls::Grid::SetColumn(languageIcon, 0);
            languageRow.Children().Append(languageIcon);

            controls::StackPanel languageLabels;
            languageLabels.Spacing(2.0);
            languageLabels.HorizontalAlignment(x::HorizontalAlignment::Left);
            languageLabels.VerticalAlignment(x::VerticalAlignment::Center);
            controls::TextBlock languageTitle;
            state->languageTitle = languageTitle;
            const auto languageTitleText = Localized(L"Language", L"\u8A00\u8A9E");
            languageTitle.Text(languageTitleText);
            languageTitle.FontSize(14.0);
            languageTitle.HorizontalAlignment(x::HorizontalAlignment::Left);
            languageTitle.TextAlignment(x::TextAlignment::Left);
            automation::AutomationProperties::SetName(languageTitle, languageTitleText);
            languageLabels.Children().Append(languageTitle);
            controls::TextBlock languageDescription;
            state->languageDescription = languageDescription;
            const auto languageDescriptionText = Localized(
                L"Choose the display language.",
                L"\u8868\u793A\u8A00\u8A9E\u3092\u9078\u629E\u3057\u307E\u3059");
            languageDescription.Text(languageDescriptionText);
            languageDescription.FontSize(12.0);
            languageDescription.TextWrapping(x::TextWrapping::Wrap);
            languageDescription.HorizontalAlignment(x::HorizontalAlignment::Left);
            languageDescription.TextAlignment(x::TextAlignment::Left);
            automation::AutomationProperties::SetName(languageDescription, languageDescriptionText);
            languageLabels.Children().Append(languageDescription);
            controls::Grid::SetColumn(languageLabels, 1);
            languageRow.Children().Append(languageLabels);

            state->language = controls::ComboBox{};
            state->language.Items().Append(winrt::box_value(Localized(L"Japanese", L"\u65E5\u672C\u8A9E")));
            state->language.Items().Append(winrt::box_value(Localized(L"English", L"\u82F1\u8A9E")));
            state->language.Width(kAppearanceSelectorWidthDips);
            state->language.MinWidth(kAppearanceSelectorWidthDips);
            state->language.HorizontalAlignment(x::HorizontalAlignment::Right);
            state->language.VerticalAlignment(x::VerticalAlignment::Center);
            automation::AutomationProperties::SetName(state->language, languageTitleText);
            state->language.SelectedIndex(ComboIndexFromLanguage(state->languageId));
            state->languageLoading = false;
            state->language.SelectionChanged([weak](auto const&, auto const&) {
                auto state = weak.lock();
                if (!state || !state->alive || state->languageLoading) return;
                const uint32_t language = LanguageFromComboIndex(state->language.SelectedIndex());
                const auto applied = ::SendMessageW(
                    state->owner, WM_LANGUAGE_CHANGED, static_cast<WPARAM>(language), 0);
                if (applied != 0)
                {
                    state->languageId = language;
                    g_uiLanguage = language;
                    ::SetWindowTextW(state->hwnd, L"Shazam");
                    ApplyLanguageToSettingsNoThrow(state);
                }
                else
                {
                    state->languageLoading = true;
                    state->language.SelectedIndex(ComboIndexFromLanguage(state->languageId));
                    state->languageLoading = false;
                }
            });
            controls::Grid::SetColumn(state->language, 2);
            languageRow.Children().Append(state->language);
            state->languageCard.Child(languageRow);
            stack.Children().Append(state->languageCard);

            state->shortcutCard = markup::XamlReader::Load(
                LR"(<Border xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation" Background="{ThemeResource CardBackgroundFillColorDefaultBrush}" CornerRadius="8" />)"
            ).as<controls::Border>();
            state->shortcutCard.Padding(
                x::ThicknessHelper::FromUniformLength(kSettingsCardPaddingDips));
            state->shortcutCard.HorizontalAlignment(x::HorizontalAlignment::Stretch);
            state->shortcutRow = controls::Grid{};
            auto shortcutRow = state->shortcutRow;
            shortcutRow.HorizontalAlignment(x::HorizontalAlignment::Stretch);
            shortcutRow.ColumnSpacing(12.0);
            auto shortcutIconColumn = controls::ColumnDefinition{};
            shortcutIconColumn.Width(x::GridLengthHelper::FromValueAndType(34.0, x::GridUnitType::Pixel));
            auto shortcutLabelColumn = controls::ColumnDefinition{};
            shortcutLabelColumn.Width(x::GridLengthHelper::FromValueAndType(1.0, x::GridUnitType::Star));
            auto shortcutControlColumn = controls::ColumnDefinition{};
            shortcutControlColumn.Width(x::GridLengthHelper::FromValueAndType(
                kSettingsControlLaneWidthDips, x::GridUnitType::Pixel));
            shortcutRow.ColumnDefinitions().Append(shortcutIconColumn);
            shortcutRow.ColumnDefinitions().Append(shortcutLabelColumn);
            shortcutRow.ColumnDefinitions().Append(shortcutControlColumn);

            controls::FontIcon shortcutIcon;
            shortcutIcon.Glyph(L"\uE765"); // KeyboardClassic
            shortcutIcon.FontSize(20.0);
            shortcutIcon.HorizontalAlignment(x::HorizontalAlignment::Left);
            shortcutIcon.VerticalAlignment(x::VerticalAlignment::Center);
            controls::Grid::SetColumn(shortcutIcon, 0);
            shortcutRow.Children().Append(shortcutIcon);

            controls::StackPanel shortcutLabels;
            shortcutLabels.Spacing(2.0);
            shortcutLabels.HorizontalAlignment(x::HorizontalAlignment::Left);
            shortcutLabels.VerticalAlignment(x::VerticalAlignment::Center);
            controls::TextBlock shortcutTitle;
            state->shortcutTitle = shortcutTitle;
            const auto shortcutTitleText = Localized(L"Shortcut", L"ショートカット");
            shortcutTitle.Text(shortcutTitleText);
            shortcutTitle.FontSize(14.0);
            shortcutTitle.HorizontalAlignment(x::HorizontalAlignment::Left);
            shortcutTitle.TextAlignment(x::TextAlignment::Left);
            automation::AutomationProperties::SetName(shortcutTitle, shortcutTitleText);
            controls::TextBlock shortcutDescription;
            state->shortcutDescription = shortcutDescription;
            const auto shortcutDescriptionText = Localized(
                L"Enter the keyboard shortcut to start Shazam from anywhere in Windows.",
                L"Windows上のどこからでも Shazam を開始したいときに使うキーボードショートカットを入力しましょう。");
            shortcutDescription.Text(shortcutDescriptionText);
            shortcutDescription.FontSize(12.0);
            shortcutDescription.TextWrapping(x::TextWrapping::Wrap);
            shortcutDescription.HorizontalAlignment(x::HorizontalAlignment::Left);
            shortcutDescription.TextAlignment(x::TextAlignment::Left);
            automation::AutomationProperties::SetName(shortcutDescription, shortcutDescriptionText);
            shortcutLabels.Children().Append(shortcutTitle);
            shortcutLabels.Children().Append(shortcutDescription);
            controls::Grid::SetColumn(shortcutLabels, 1);
            shortcutRow.Children().Append(shortcutLabels);

            state->shortcutButton = controls::Button{};
            state->shortcutButton.Content(winrt::box_value(FormatShortcut(
                state->shortcutModifiers, state->shortcutKey)));
            state->shortcutButton.MinWidth(144.0);
            state->shortcutButton.HorizontalAlignment(x::HorizontalAlignment::Right);
            state->shortcutButton.VerticalAlignment(x::VerticalAlignment::Center);
            automation::AutomationProperties::SetName(state->shortcutButton, shortcutTitleText);
            controls::Grid::SetColumn(state->shortcutButton, 2);
            shortcutRow.Children().Append(state->shortcutButton);

            state->shortcutInfoBar = controls::InfoBar{};
            state->shortcutInfoBar.IsClosable(false);
            state->shortcutInfoBar.IsOpen(false);
            state->shortcutInfoBar.Visibility(x::Visibility::Collapsed);
            state->shortcutInfoBar.Margin(x::ThicknessHelper::FromLengths(
                0.0, kSettingsInlineMessageSpacingDips, 0.0, 0.0));
            state->shortcutInfoBar.Severity(controls::InfoBarSeverity::Error);

            state->shortcutButton.Click([weak](auto const&, auto const&) {
                auto state = weak.lock();
                if (!state || !state->alive || state->shortcutRecording) return;
                state->shortcutRecording = true;
                state->shortcutInfoBar.IsOpen(false);
                state->shortcutInfoBar.Visibility(x::Visibility::Collapsed);
                state->shortcutButton.Content(winrt::box_value(
                    Localized(L"Press a key combination…", L"キーの組み合わせを入力…")));
                const auto captureStarted = state->owner && ::IsWindow(state->owner)
                    ? ::SendMessageW(state->owner, WM_GLOBAL_SHORTCUT_CAPTURE, TRUE, 0)
                    : 0;
                if (captureStarted == 0)
                {
                    state->shortcutInfoBar.Title(Localized(
                        L"Shortcut capture could not start", L"ショートカットの入力を開始できません"));
                    state->shortcutInfoBar.Message(Localized(
                        L"Try opening Settings again.", L"設定画面を開き直してください。"));
                    state->shortcutInfoBar.Visibility(x::Visibility::Visible);
                    state->shortcutInfoBar.IsOpen(true);
                    EndShortcutCaptureNoThrow(state);
                    return;
                }
                state->shortcutButton.Focus(x::FocusState::Programmatic);
            });
            state->shortcutButton.KeyDown([weak](auto const&, auto const& args) {
                auto state = weak.lock();
                if (!state || !state->alive || !state->shortcutRecording) return;
                const auto key = static_cast<uint32_t>(args.Key());
                if (key == VK_ESCAPE &&
                    (::GetKeyState(VK_CONTROL) & 0x8000) == 0 &&
                    (::GetKeyState(VK_MENU) & 0x8000) == 0 &&
                    (::GetKeyState(VK_SHIFT) & 0x8000) == 0 &&
                    (::GetKeyState(VK_LWIN) & 0x8000) == 0 &&
                    (::GetKeyState(VK_RWIN) & 0x8000) == 0)
                {
                    EndShortcutCaptureNoThrow(state);
                    args.Handled(true);
                    return;
                }
                if (key == VK_SHIFT || key == VK_CONTROL || key == VK_MENU ||
                    key == VK_LWIN || key == VK_RWIN ||
                    (key >= VK_LSHIFT && key <= VK_RMENU))
                {
                    args.Handled(true);
                    return;
                }

                uint32_t modifiers = 0;
                if ((::GetKeyState(VK_CONTROL) & 0x8000) != 0) modifiers |= MOD_CONTROL;
                if ((::GetKeyState(VK_MENU) & 0x8000) != 0) modifiers |= MOD_ALT;
                if ((::GetKeyState(VK_SHIFT) & 0x8000) != 0) modifiers |= MOD_SHIFT;
                if ((::GetKeyState(VK_LWIN) & 0x8000) != 0 ||
                    (::GetKeyState(VK_RWIN) & 0x8000) != 0) modifiers |= MOD_WIN;

                const auto result = state->owner && ::IsWindow(state->owner)
                    ? ::SendMessageW(state->owner, WM_GLOBAL_SHORTCUT_CHANGED,
                        static_cast<WPARAM>(modifiers), static_cast<LPARAM>(key))
                    : 0;
                if (result != 0)
                {
                    state->shortcutModifiers = modifiers;
                    state->shortcutKey = key;
                    state->shortcutButton.Content(winrt::box_value(
                        FormatShortcut(modifiers, key)));
                    EndShortcutCaptureNoThrow(state);
                }
                else
                {
                    state->shortcutInfoBar.Title(Localized(
                        L"Shortcut is unavailable", L"このショートカットは使用できません"));
                    state->shortcutInfoBar.Message(Localized(
                        L"Choose a different key combination.",
                        L"別のキーの組み合わせを指定してください。"));
                    state->shortcutInfoBar.Visibility(x::Visibility::Visible);
                    state->shortcutInfoBar.IsOpen(true);
                    EndShortcutCaptureNoThrow(state);
                }
                args.Handled(true);
            });
            state->shortcutButton.LostFocus([weak](auto const&, auto const&) {
                if (auto state = weak.lock(); state && state->alive)
                    EndShortcutCaptureNoThrow(state);
            });
            state->shortcutCard.Child(shortcutRow);
            controls::StackPanel shortcutSection;
            shortcutSection.Spacing(0.0);
            shortcutSection.Children().Append(state->shortcutCard);
            shortcutSection.Children().Append(state->shortcutInfoBar);
            stack.Children().Append(shortcutSection);

            // Leave page surfaces transparent so NavigationView's stock
            // content surface and theme resources remain visible.
            state->settingsViewer = markup::XamlReader::Load(
                LR"(<ScrollViewer xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation" />)"
            ).as<controls::ScrollViewer>();
            state->settingsViewer.Content(stack);
            state->settingsViewer.VerticalScrollBarVisibility(controls::ScrollBarVisibility::Auto);
            state->settingsViewer.HorizontalScrollBarVisibility(controls::ScrollBarVisibility::Disabled);

            state->historyViewer = markup::XamlReader::Load(
                LR"(<ScrollViewer xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation" />)"
            ).as<controls::ScrollViewer>();
            state->historyViewer.VerticalScrollBarVisibility(controls::ScrollBarVisibility::Auto);
            state->historyViewer.HorizontalScrollBarVisibility(controls::ScrollBarVisibility::Disabled);
            state->historyContent = controls::StackPanel{};
            state->historyContent.Spacing(12.0);
            state->historyContent.Padding(x::ThicknessHelper::FromLengths(
                kSettingsContentGutterDips, 24.0, kSettingsContentGutterDips, 24.0));

            controls::TextBlock historyTitle;
            state->historyTitle = historyTitle;
            const auto historyTitleText = Localized(L"History", L"\u5C65\u6B74");
            historyTitle.Text(historyTitleText);
            historyTitle.FontSize(28.0);
            historyTitle.FontWeight(winrt::Windows::UI::Text::FontWeights::Bold());
            historyTitle.HorizontalAlignment(x::HorizontalAlignment::Left);
            historyTitle.Margin(x::ThicknessHelper::FromLengths(0.0, 0.0, 0.0, 8.0));
            automation::AutomationProperties::SetName(historyTitle, historyTitleText);
            state->historyContent.Children().Append(historyTitle);

            state->historyEmpty = controls::TextBlock{};
            state->historyEmpty.Text(Localized(
                L"No recognized songs",
                L"\u307E\u3060\u8A8D\u8B58\u3057\u305F\u66F2\u306F\u3042\u308A\u307E\u305B\u3093"));
            state->historyEmpty.FontSize(14.0);
            state->historyEmpty.Opacity(0.72);
            state->historyEmpty.Margin(x::ThicknessHelper::FromLengths(0.0, 12.0, 0.0, 0.0));
            automation::AutomationProperties::SetName(state->historyEmpty,
                state->historyEmpty.Text());
            state->historyContent.Children().Append(state->historyEmpty);

            state->historyItems = controls::StackPanel{};
            state->historyItems.Spacing(8.0);
            state->historyContent.Children().Append(state->historyItems);
            state->historyViewer.Content(state->historyContent);
            state->historyViewer.Visibility(x::Visibility::Collapsed);

            state->pageHost = controls::Grid{};
            state->pageHost.Children().Append(state->settingsViewer);
            state->pageHost.Children().Append(state->historyViewer);

            // The root fills the XAML island with WinUI's themed page surface.
            // NavigationView can then use its own default pane and content
            // resources instead of inheriting the native host's light color.
            state->shellRoot = markup::XamlReader::Load(
                LR"(<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation" Background="{ThemeResource ApplicationPageBackgroundThemeBrush}" />)"
            ).as<controls::Grid>();
            if (state->customTitleBarActive)
            {
                state->titleBar = markup::XamlReader::Load(
                    LR"(<TitleBar xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation" />)"
                ).as<controls::TitleBar>();
                state->titleBar.Title(Localized(L"Shazam", L"Shazam"));
                state->titleBar.IsBackButtonVisible(true);
                state->titleBar.IsBackButtonEnabled(true);
                // Keep NavigationView's stock pane toggle in its own row.
                state->titleBar.IsPaneToggleButtonVisible(false);
                controls::BitmapIconSource iconSource;
                iconSource.UriSource(GetTitleBarIconUri());
                iconSource.ShowAsMonochrome(false);
                state->titleBar.IconSource(iconSource);
                state->titleBar.BackRequested([weak](auto const&, auto const&) {
                    if (auto state = weak.lock(); state && state->alive)
                    {
                        if (state->historyViewer.Visibility() == x::Visibility::Visible)
                            state->navigation.SelectedItem(state->navigation.SettingsItem());
                        else
                            ::SendMessageW(state->hwnd, WM_CLOSE, 0, 0);
                    }
                });

                const auto titleBarHeight = state->appWindowTitleBar.Height();
                if (titleBarHeight > 0)
                {
                    auto titleBarDpi = ::GetDpiForWindow(state->hwnd);
                    if (titleBarDpi < 96) titleBarDpi = 96;
                    state->titleBar.Height(
                        static_cast<double>(::MulDiv(titleBarHeight, 96, titleBarDpi)));
                }
                auto titleRow = controls::RowDefinition{};
                titleRow.Height(x::GridLengthHelper::FromValueAndType(
                    1.0, x::GridUnitType::Auto));
                auto navigationRow = controls::RowDefinition{};
                navigationRow.Height(x::GridLengthHelper::FromValueAndType(
                    1.0, x::GridUnitType::Star));
                state->shellRoot.RowDefinitions().Append(titleRow);
                state->shellRoot.RowDefinitions().Append(navigationRow);
                state->shellRoot.Children().Append(state->titleBar);
                state->titleBar.Loaded([weak](auto const&, auto const&) {
                    if (auto state = weak.lock(); state && state->alive)
                    {
                        state->titleBar.ApplyTemplate();
                        state->titleBar.UpdateLayout();
                        UpdateTitleBarDragRegions(state);
                    }
                });
                state->titleBar.SizeChanged([weak](auto const&, auto const&) {
                    if (auto state = weak.lock(); state && state->alive)
                        UpdateTitleBarDragRegions(state);
                });
            }
            state->navigation = markup::XamlReader::Load(
                LR"(<NavigationView xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation" />)"
            ).as<controls::NavigationView>();
            state->navigation.PaneDisplayMode(controls::NavigationViewPaneDisplayMode::LeftCompact);
            state->navigation.IsBackButtonVisible(state->customTitleBarActive
                ? controls::NavigationViewBackButtonVisible::Collapsed
                : controls::NavigationViewBackButtonVisible::Visible);
            state->navigation.IsBackEnabled(true);
            // NavigationView's built-in Settings destination belongs in the
            // footer, matching the standard Windows navigation silhouette.
            state->navigation.IsSettingsVisible(true);

            state->historyNavigationItem = controls::NavigationViewItem{};
            state->historyNavigationItem.Content(winrt::box_value(historyTitleText));
            controls::FontIcon historyIcon;
            historyIcon.Glyph(L"\uE81C");
            state->historyNavigationItem.Icon(historyIcon);
            state->historyNavigationItem.Tag(winrt::box_value(1));
            automation::AutomationProperties::SetName(
                state->historyNavigationItem, historyTitleText);

            state->navigation.MenuItems().Append(state->historyNavigationItem);
            if (auto settingsItem = state->navigation.SettingsItem()
                    .try_as<controls::NavigationViewItem>())
            {
                const auto settingsText = Localized(L"Settings", L"\u8A2D\u5B9A");
                settingsItem.Content(winrt::box_value(settingsText));
                automation::AutomationProperties::SetName(settingsItem, settingsText);
            }
            state->navigation.Content(state->pageHost);
            state->navigation.SelectionChanged([weak](auto const&, auto const& args) {
                auto state = weak.lock();
                if (!state || !state->alive) return;
                auto selected = args.SelectedItemContainer().try_as<controls::NavigationViewItem>();
                const bool showSettings = args.IsSettingsSelected();
                const int32_t page = !showSettings && selected
                    ? winrt::unbox_value_or<int32_t>(selected.Tag(), 0)
                    : 0;
                const bool showHistory = !showSettings && page == 1;
                state->settingsViewer.Visibility(showHistory
                    ? x::Visibility::Collapsed : x::Visibility::Visible);
                state->historyViewer.Visibility(showHistory
                    ? x::Visibility::Visible : x::Visibility::Collapsed);
                if (showHistory) RefreshHistoryNoThrow(state);
            });
            state->navigation.BackRequested([weak](auto const&, auto const&) {
                if (auto state = weak.lock(); state && state->alive)
                {
                    if (state->historyViewer.Visibility() == x::Visibility::Visible)
                        state->navigation.SelectedItem(state->navigation.SettingsItem());
                    else
                        ::SendMessageW(state->hwnd, WM_CLOSE, 0, 0);
                }
            });
            state->navigation.PaneOpened([weak](auto const&, auto const&) {
                if (auto state = weak.lock(); state && state->alive)
                    UpdateSettingsCardWidths(state, state->navigation.ActualWidth());
            });
            state->navigation.PaneClosed([weak](auto const&, auto const&) {
                if (auto state = weak.lock(); state && state->alive)
                    UpdateSettingsCardWidths(state, state->navigation.ActualWidth());
            });
            state->navigation.SelectedItem(state->navigation.SettingsItem());
            if (state->customTitleBarActive)
                controls::Grid::SetRow(state->navigation, 1);
            state->shellRoot.Children().Append(state->navigation);
            ApplyAppearanceToSettings(state, state->appearanceMode);
            return state->shellRoot;
        }

        void Layout()
        {
            if (!hwnd || !island) return;
            RECT rect{};
            ::GetClientRect(hwnd, &rect);
            auto dpi = ::GetDpiForWindow(hwnd);
            if (dpi < 96) dpi = 96;
            if (state)
            {
                UpdateSettingsCardWidths(state,
                    static_cast<double>(::MulDiv(rect.right - rect.left, 96, dpi)));
                if (state->titleBar)
                {
                    const auto titleBarHeight = state->appWindowTitleBar.Height();
                    if (titleBarHeight > 0)
                        state->titleBar.Height(
                            static_cast<double>(::MulDiv(titleBarHeight, 96, dpi)));
                    state->titleBar.UpdateLayout();
                    UpdateTitleBarDragRegions(state);
                }
            }
            island.SiteBridge().MoveAndResize({0, 0, rect.right - rect.left, rect.bottom - rect.top});
        }

        void Show(
            uint32_t appearanceMode,
            uint32_t shortcutModifiers,
            uint32_t shortcutKey,
            uint32_t languageId)
        {
            if (!hwnd) return;
            if (state)
            {
                g_uiLanguage = languageId == kLanguageJapanese
                    ? kLanguageJapanese : kLanguageEnglish;
                state->languageId = g_uiLanguage;
                ApplyLanguageToSettingsNoThrow(state);
                state->appearanceLoading = true;
                state->appearance.SelectedIndex(ComboIndexFromAppearance(appearanceMode));
                state->appearanceLoading = false;
                ApplyAppearanceToSettings(state, appearanceMode);
                EndShortcutCaptureNoThrow(state);
                state->shortcutModifiers = shortcutModifiers;
                state->shortcutKey = shortcutKey;
                state->shortcutButton.Content(winrt::box_value(FormatShortcut(
                    state->shortcutModifiers, state->shortcutKey)));
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
                if (state) EndShortcutCaptureNoThrow(state);
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
            case WM_GETMINMAXINFO:
                if (lp)
                {
                    auto* limits = reinterpret_cast<MINMAXINFO*>(lp);
                    auto dpi = ::GetDpiForWindow(h);
                    if (dpi < 96) dpi = 96;
                    RECT minimumClient{
                        0,
                        0,
                        ::MulDiv(kMinimumClientWidthDips, dpi, 96),
                        ::MulDiv(kMinimumClientHeightDips, dpi, 96),
                    };
                    const auto style = static_cast<DWORD>(::GetWindowLongPtrW(h, GWL_STYLE));
                    const auto exStyle = static_cast<DWORD>(::GetWindowLongPtrW(h, GWL_EXSTYLE));
                    if (::AdjustWindowRectExForDpi(&minimumClient, style, FALSE, exStyle, dpi))
                    {
                        limits->ptMinTrackSize.x = minimumClient.right - minimumClient.left;
                        limits->ptMinTrackSize.y = minimumClient.bottom - minimumClient.top;
                    }
                    else
                    {
                        limits->ptMinTrackSize.x = minimumClient.right;
                        limits->ptMinTrackSize.y = minimumClient.bottom;
                    }
                }
                return 0;
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
                if (host && host->hwnd == h)
                    EndShortcutCaptureNoThrow(host->state);
                // While Settings is visible, the owner popup deliberately
                // ignores its activation loss so it stays open behind this
                // window. Restore the owner's activation before hiding
                // Settings so the next outside click reaches the normal
                // popup deactivation path and dismisses the main UI.
                if (host && host->hwnd == h && host->owner &&
                    ::IsWindowVisible(host->owner))
                {
                    if (!::SetForegroundWindow(host->owner))
                    {
                        ::SetActiveWindow(host->owner);
                        ::SetFocus(host->owner);
                    }
                }
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
    HWND owner, uint32_t appearanceMode, uint32_t shortcutModifiers,
    uint32_t shortcutKey, uint32_t languageId) noexcept
{
    try
    {
        if (!owner || !::IsWindow(owner)) return E_INVALIDARG;
        if (!g_settings)
        {
            auto host = std::make_unique<SettingsHost>();
            g_active = host.get();
            try { host->Initialize(owner, languageId); }
            catch (...) { host->Close(); g_active = nullptr; throw; }
            g_settings = std::move(host);
        }
        g_settings->Show(appearanceMode, shortcutModifiers, shortcutKey, languageId);
        return S_OK;
    }
    catch (winrt::hresult_error const& e) { return e.code(); }
    catch (...) { return E_FAIL; }
}

extern "C" __declspec(dllexport) void __stdcall OpenShazamSettingsSetHistory(
    OpenShazamHistoryItem const* items, size_t count) noexcept
{
    if (g_settings)
        SetHistoryNoThrow(g_settings->state, items, count);
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
