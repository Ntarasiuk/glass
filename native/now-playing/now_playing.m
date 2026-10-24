// What the Mac is playing (any app: Spotify, Music, a browser…), for the viewer's media
// controls. macOS 15.4+ only answers MediaRemote for Apple's own processes, so Glass loads
// this library into /usr/bin/perl (see crates/glass-host/src/now_playing.rs):
//   - prints one JSON line per change: {"title","artist","album","playing","duration",
//     "elapsed","rate","artworkId","artwork" (base64 JPEG/PNG, only when it changes)}
//   - reads commands from stdin, one per line: play, pause, toggle, next, previous,
//     seek <seconds>
// Built with: clang -dynamiclib -fobjc-arc -framework Foundation now_playing.m

#import <Foundation/Foundation.h>
#include <dlfcn.h>

typedef void (*GetInfo)(dispatch_queue_t, void (^)(NSDictionary *));
typedef void (*Register)(dispatch_queue_t);
typedef Boolean (*SendCommand)(int, NSDictionary *);
typedef void (*SetElapsed)(double);

static GetInfo getInfo;
static SendCommand sendCommand;
static SetElapsed setElapsed;
static NSString *lastLine;
static NSString *lastArtworkId;

static void report(void)
{
    getInfo(dispatch_get_main_queue(), ^(NSDictionary *info) {
        NSMutableDictionary *out = [NSMutableDictionary dictionary];
        NSString *(^str)(NSString *) = ^NSString *(NSString *key) {
            id v = info[[@"kMRMediaRemoteNowPlayingInfo" stringByAppendingString:key]];
            return [v isKindOfClass:[NSString class]] ? v : nil;
        };
        NSNumber *(^num)(NSString *) = ^NSNumber *(NSString *key) {
            id v = info[[@"kMRMediaRemoteNowPlayingInfo" stringByAppendingString:key]];
            return [v isKindOfClass:[NSNumber class]] ? v : nil;
        };
        if (str(@"Title")) out[@"title"] = str(@"Title");
        if (str(@"Artist")) out[@"artist"] = str(@"Artist");
        if (str(@"Album")) out[@"album"] = str(@"Album");
        if (num(@"Duration")) out[@"duration"] = num(@"Duration");
        if (num(@"ElapsedTime")) out[@"elapsed"] = num(@"ElapsedTime");
        NSNumber *rate = num(@"PlaybackRate");
        out[@"rate"] = rate ?: @0;
        out[@"playing"] = @(rate.doubleValue > 0);
        id stamp = info[@"kMRMediaRemoteNowPlayingInfoTimestamp"];
        if ([stamp isKindOfClass:[NSDate class]]) out[@"at"] = @((long long)([(NSDate *)stamp timeIntervalSince1970] * 1000));
        NSString *artId = str(@"ArtworkIdentifier") ?: str(@"Title");
        out[@"artworkId"] = artId ?: @"";
        // Compare without the artwork, then add it only when it changed.
        NSData *json = [NSJSONSerialization dataWithJSONObject:out options:NSJSONWritingSortedKeys error:nil];
        NSString *line = [[NSString alloc] initWithData:json encoding:NSUTF8StringEncoding];
        if ([line isEqualToString:lastLine]) return;
        lastLine = line;
        id art = info[@"kMRMediaRemoteNowPlayingInfoArtworkData"];
        if ([art isKindOfClass:[NSData class]] && artId && ![artId isEqualToString:lastArtworkId] && [(NSData *)art length] < 2000000) {
            lastArtworkId = artId;
            out[@"artwork"] = [(NSData *)art base64EncodedStringWithOptions:0];
            out[@"artworkType"] = str(@"ArtworkMIMEType") ?: @"image/jpeg";
            json = [NSJSONSerialization dataWithJSONObject:out options:NSJSONWritingSortedKeys error:nil];
        }
        fwrite(json.bytes, 1, json.length, stdout);
        fputc('\n', stdout);
        fflush(stdout);
    });
}

// Runs until stdin closes (Glass stopped it, or exited).
void glass_now_playing(void)
{
    void *mr = dlopen("/System/Library/PrivateFrameworks/MediaRemote.framework/MediaRemote", RTLD_NOW);
    if (!mr) return;
    getInfo = (GetInfo)dlsym(mr, "MRMediaRemoteGetNowPlayingInfo");
    sendCommand = (SendCommand)dlsym(mr, "MRMediaRemoteSendCommand");
    setElapsed = (SetElapsed)dlsym(mr, "MRMediaRemoteSetElapsedTime");
    Register reg = (Register)dlsym(mr, "MRMediaRemoteRegisterForNowPlayingNotifications");
    if (!getInfo) return;
    if (reg) reg(dispatch_get_main_queue());
    for (NSString *name in @[@"kMRMediaRemoteNowPlayingInfoDidChangeNotification",
             @"kMRMediaRemoteNowPlayingApplicationIsPlayingDidChangeNotification",
             @"kMRMediaRemoteNowPlayingApplicationDidChangeNotification"]) {
        [[NSNotificationCenter defaultCenter] addObserverForName:name object:nil queue:nil usingBlock:^(NSNotification *n) { report(); }];
    }
    // Some players don't post changes reliably; a cheap check every second catches them.
    [NSTimer scheduledTimerWithTimeInterval:1.0 repeats:YES block:^(NSTimer *t) { report(); }];
    report();

    dispatch_async(dispatch_get_global_queue(0, 0), ^{
        char buf[256];
        while (fgets(buf, sizeof buf, stdin)) {
            NSString *cmd = [[NSString stringWithUTF8String:buf] stringByTrimmingCharactersInSet:NSCharacterSet.whitespaceAndNewlineCharacterSet];
            dispatch_async(dispatch_get_main_queue(), ^{
                // MRMediaRemoteCommand: play 0, pause 1, toggle 2, next 4, previous 5.
                if ([cmd isEqualToString:@"play"] && sendCommand) sendCommand(0, nil);
                else if ([cmd isEqualToString:@"pause"] && sendCommand) sendCommand(1, nil);
                else if ([cmd isEqualToString:@"toggle"] && sendCommand) sendCommand(2, nil);
                else if ([cmd isEqualToString:@"next"] && sendCommand) sendCommand(4, nil);
                else if ([cmd isEqualToString:@"previous"] && sendCommand) sendCommand(5, nil);
                else if ([cmd hasPrefix:@"seek "] && setElapsed) setElapsed([[cmd substringFromIndex:5] doubleValue]);
                report();
            });
        }
        exit(0);
    });
    CFRunLoopRun();
}
