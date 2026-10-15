#pragma once
#include "App.xaml.g.h"
namespace winrt::OpenShazam::SettingsHost::implementation
{
    struct App : AppT<App>
    {
        App();
        void OnLaunched(winrt::Microsoft::UI::Xaml::LaunchActivatedEventArgs const&) {}
    };
}
namespace winrt::OpenShazam::SettingsHost::factory_implementation
{
    struct App : AppT<App, implementation::App> {};
}
