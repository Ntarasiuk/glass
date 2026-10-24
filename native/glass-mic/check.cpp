// CI check for GlassMicrophone.driver without installing it: load the plug-in, call its
// entry point, and ask it for its device the way the audio server would.
//   clang++ -std=c++17 check.cpp -framework CoreFoundation -framework CoreAudio -o check
//   ./check GlassMicrophone.driver/Contents/MacOS/GlassMicrophone

#include <CoreAudio/AudioServerPlugIn.h>
#include <dlfcn.h>

#include <cstdio>
#include <cstring>
#include <initializer_list>

#define CHECK(cond, ...) \
    if (!(cond)) { std::fprintf(stderr, __VA_ARGS__); std::fputc('\n', stderr); return 1; }

int main(int argc, char** argv)
{
    CHECK(argc == 2, "usage: check <driver binary>");
    void* lib = dlopen(argv[1], RTLD_NOW);
    CHECK(lib, "dlopen: %s", dlerror());
    using Entry = void* (*)(CFAllocatorRef, CFUUIDRef);
    auto entry = reinterpret_cast<Entry>(dlsym(lib, "GlassMicrophoneEntryPoint"));
    CHECK(entry, "no GlassMicrophoneEntryPoint");
    auto driver = static_cast<AudioServerPlugInDriverRef>(entry(kCFAllocatorDefault, kAudioServerPlugInTypeUUID));
    CHECK(driver, "the entry point returned no driver");

    void* iface = nullptr;
    CHECK((*driver)->QueryInterface(driver, CFUUIDGetUUIDBytes(kAudioServerPlugInDriverInterfaceUUID), &iface) == 0 && iface,
        "no driver interface");

    AudioObjectPropertyAddress list = {kAudioPlugInPropertyDeviceList, kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyElementMain};
    UInt32 size = 0;
    (*driver)->GetPropertyDataSize(driver, kAudioObjectPlugInObject, 0, &list, 0, nullptr, &size);
    CHECK(size == sizeof(AudioObjectID), "expected one device, got %u", size / unsigned(sizeof(AudioObjectID)));
    AudioObjectID device = 0;
    (*driver)->GetPropertyData(driver, kAudioObjectPlugInObject, 0, &list, 0, nullptr, sizeof(device), &size, &device);

    AudioObjectPropertyAddress name = {kAudioObjectPropertyName, kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyElementMain};
    CFStringRef cfName = nullptr;
    (*driver)->GetPropertyData(driver, device, 0, &name, 0, nullptr, sizeof(cfName), &size, &cfName);
    char text[128] = {0};
    CHECK(cfName && CFStringGetCString(cfName, text, sizeof(text), kCFStringEncodingUTF8), "no device name");
    CHECK(std::strcmp(text, "Glass Microphone") == 0, "device is named \"%s\"", text);

    for (auto scope : {kAudioObjectPropertyScopeInput, kAudioObjectPropertyScopeOutput}) {
        AudioObjectPropertyAddress streams = {kAudioDevicePropertyStreams, scope, kAudioObjectPropertyElementMain};
        (*driver)->GetPropertyDataSize(driver, device, 0, &streams, 0, nullptr, &size);
        CHECK(size == sizeof(AudioObjectID), "expected one %s stream", scope == kAudioObjectPropertyScopeInput ? "input" : "output");
    }
    std::printf("Glass Microphone: loads, one device with an input and an output stream\n");
    return 0;
}
