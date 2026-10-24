// Glass Microphone: a virtual audio device for macOS. What Glass plays into its output
// comes out of its input, so apps on the Mac can use a viewer's microphone like any other
// mic. Built on libASPL (MIT); loaded by the system's audio server (coreaudiod).

#include <aspl/Driver.hpp>

#include <CoreAudio/AudioServerPlugIn.h>

#include <atomic>
#include <cstring>
#include <vector>

namespace {

constexpr UInt32 SampleRate = 48000;
constexpr UInt32 ChannelCount = 2;
constexpr UInt32 BytesPerFrame = ChannelCount * sizeof(SInt16);
// About 2.7 s of audio, addressed by sample time: a write for time T is read back by the
// input at time T (BlackHole's approach), and read slots are cleared so silence follows
// when Glass stops writing.
constexpr UInt64 RingFrames = 131072;

class Loopback : public aspl::IORequestHandler
{
public:
    Loopback() : ring_(RingFrames * BytesPerFrame, 0) {}

    void OnWriteMixedOutput(const std::shared_ptr<aspl::Stream>&,
        Float64,
        Float64 timestamp,
        const void* bytes,
        UInt32 bytesCount) override
    {
        Copy(static_cast<UInt64>(timestamp), bytesCount, [&](size_t ringAt, size_t at, size_t n) {
            std::memcpy(&ring_[ringAt], static_cast<const char*>(bytes) + at, n);
        });
    }

    void OnReadClientInput(const std::shared_ptr<aspl::Client>&,
        const std::shared_ptr<aspl::Stream>&,
        Float64,
        Float64 timestamp,
        void* bytes,
        UInt32 bytesCount) override
    {
        Copy(static_cast<UInt64>(timestamp), bytesCount, [&](size_t ringAt, size_t at, size_t n) {
            std::memcpy(static_cast<char*>(bytes) + at, &ring_[ringAt], n);
            std::memset(&ring_[ringAt], 0, n);
        });
    }

private:
    // Calls f(ring offset, buffer offset, length) for the (at most two) contiguous parts.
    template <typename F> void Copy(UInt64 frame, UInt32 bytesCount, F f)
    {
        size_t start = (frame % RingFrames) * BytesPerFrame;
        size_t size = ring_.size();
        size_t first = std::min<size_t>(bytesCount, size - start);
        f(start, 0, first);
        if (first < bytesCount) {
            f(0, first, bytesCount - first);
        }
    }

    std::vector<char> ring_;
};

std::shared_ptr<aspl::Driver> CreateDriver()
{
    auto context = std::make_shared<aspl::Context>();

    aspl::DeviceParameters params;
    params.Name = "Glass Microphone";
    params.Manufacturer = "Glass";
    params.DeviceUID = "dev.glassdesk.microphone";
    params.ModelUID = "dev.glassdesk.microphone.model";
    params.SampleRate = SampleRate;
    params.ChannelCount = ChannelCount;
    // A microphone: fine as the default input, never as the speaker for alert sounds.
    params.CanBeDefault = true;
    params.CanBeDefaultForSystemSounds = false;

    auto device = std::make_shared<aspl::Device>(context, params);
    device->AddStreamWithControlsAsync(aspl::Direction::Input);
    device->AddStreamAsync(aspl::Direction::Output);
    device->SetIOHandler(std::make_shared<Loopback>());

    auto plugin = std::make_shared<aspl::Plugin>(context);
    plugin->AddDevice(device);
    return std::make_shared<aspl::Driver>(context, plugin);
}

} // namespace

extern "C" void* GlassMicrophoneEntryPoint(CFAllocatorRef, CFUUIDRef typeUUID)
{
    if (!CFEqual(typeUUID, kAudioServerPlugInTypeUUID)) {
        return nullptr;
    }
    static std::shared_ptr<aspl::Driver> driver = CreateDriver();
    return driver->GetReference();
}
