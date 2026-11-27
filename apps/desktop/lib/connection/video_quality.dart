enum VideoProfile {
  auto('自动'),
  clear('文字清晰'),
  smooth('操作流畅');

  const VideoProfile(this.label);

  final String label;

  static VideoProfile parse(Object? value) =>
      values.firstWhere((profile) => profile.name == value, orElse: () => auto);
}

enum StreamResolution {
  native(null, '按远端尺寸', 0, 0),
  hd('hd', '720p', 1280, 720),
  fullHd('full_hd', '1080p', 1920, 1080),
  qhd('qhd', '1440p · 2K', 2560, 1440),
  uhd('uhd', '2160p · 4K', 3840, 2160);

  const StreamResolution(this.value, this.label, this.longSide, this.shortSide);

  final String? value;
  final String label;

  final int longSide, shortSide;

  bool limits(int width, int height) =>
      this != native &&
      (width >= height
          ? width > longSide || height > shortSide
          : height > longSide || width > shortSide);

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
    fps: fps,
    resolution: resolution,
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
