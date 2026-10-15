#include "pch.h"
#include "App.xaml.h"
#if __has_include("App.g.cpp")
#include "App.g.cpp"
#endif
namespace winrt::OpenShazam::SettingsHost::implementation
{
    App::App() { InitializeComponent(); }
}
