enum VideoProfile {
  auto('自动', 30, StreamResolution.native),
  clear('文字清晰', 30, StreamResolution.qhd),
  smooth('操作流畅', 60, StreamResolution.fullHd);

  const VideoProfile(this.label, this.defaultFps, this.defaultResolution);

  final String label;
  final int defaultFps;
  final StreamResolution defaultResolution;

  static VideoProfile parse(Object? value) =>
      values.firstWhere((profile) => profile.name == value, orElse: () => auto);
}

enum StreamResolution {
  native(null, '按远端尺寸'),
  hd('hd', '720p'),
  fullHd('full_hd', '1080p'),
  qhd('qhd', '1440p · 2K'),
  uhd('uhd', '2160p · 4K');

  const StreamResolution(this.value, this.label);

  final String? value;
  final String label;

  static StreamResolution parse(Object? value) => values.firstWhere(
    (resolution) => resolution.value == value,
    orElse: () => native,
  );
}

enum DesktopVideoCodec {
  auto(null, '自动（优先 H.264）'),
  h264('h264', 'H.264'),
  av1('av1', 'AV1（可用时）'),
  vp8('vp8', 'VP8');

  const DesktopVideoCodec(this.value, this.label);

  final String? value;
  final String label;

  static DesktopVideoCodec parse(Object? value) =>
      values.firstWhere((codec) => codec.value == value, orElse: () => auto);
}

final class VideoQuality {
  const VideoQuality({
    this.profile = VideoProfile.auto,
    this.fps = 30,
    this.resolution = StreamResolution.native,
    this.maxMbps,
  });

  factory VideoQuality.fromJson(Map<String, Object?> json) {
    final fps = json['fps'];
    final maxMbps = json['max_mbps'];
    return VideoQuality(
      profile: VideoProfile.parse(json['preset']),
      fps: fps is int && fps >= 10 && fps <= 60 ? fps : 30,
      resolution: StreamResolution.parse(json['resolution']),
      maxMbps: maxMbps is int && maxMbps >= 1 && maxMbps <= 80 ? maxMbps : null,
    );
  }

  final VideoProfile profile;
  final int fps;
  final StreamResolution resolution;
  final int? maxMbps;

  bool get isDefault =>
      profile == VideoProfile.auto &&
      fps == 30 &&
      resolution == StreamResolution.native &&
      maxMbps == null;

  VideoQuality withProfile(VideoProfile next) => VideoQuality(
    profile: next,
    fps: next.defaultFps,
    resolution: next.defaultResolution,
    maxMbps: maxMbps,
  );

  VideoQuality withCaps({int? fps, StreamResolution? resolution}) =>
      VideoQuality(
        profile: profile,
        fps: fps ?? this.fps,
        resolution: resolution ?? this.resolution,
        maxMbps: maxMbps,
      );

  VideoQuality withMaxMbps(int? maxMbps) => VideoQuality(
    profile: profile,
    fps: fps,
    resolution: resolution,
    maxMbps: maxMbps,
  );

  bool matches(VideoQuality other) =>
      other.profile == profile &&
      other.fps == fps &&
      other.resolution == resolution &&
      other.maxMbps == maxMbps;

  Map<String, Object?> toJson() => {
    'preset': profile.name,
    'fps': fps,
    'resolution': ?resolution.value,
    'max_mbps': ?maxMbps,
  };
}
