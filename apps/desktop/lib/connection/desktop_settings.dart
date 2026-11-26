import 'directory_origin.dart';
import 'session_permissions.dart';
import 'video_quality.dart';

enum ApprovalPolicy {
  manual('manual', '每次确认', '每次连接由本机批准。'),
  trustedDevices(
    'trusted_devices',
    '可信设备免确认',
    '可信设备可在 30 天内按已批准的权限接入，其他设备仍可申请批准。',
  ),
  password('password', '密码验证后连接', '通过连接密码验证后，按本机允许的权限接入。'),
  trustedOnly(
    'trusted_only',
    '仅允许已授权设备',
    '只接受密码和设备身份均通过验证的可信设备，连接权限限于此前批准的范围。',
  );

  const ApprovalPolicy(this.value, this.label, this.description);
  final String value, label, description;

  static ApprovalPolicy fromValue(Object? value) => values.firstWhere(
    (policy) => policy.value == value,
    orElse: () => manual,
  );
}

final class DesktopSettings {
  const DesktopSettings({
    this.useSelfHosted = false,
    this.directory = '',
    this.forceRelay = false,
    this.lanAllowed = true,
    this.address = '',
    this.port = 53980,
    this.source = '',
    this.shareRoot = '',
    this.allowShell = false,
    this.approvalPolicy = ApprovalPolicy.manual,
    this.downloadRoot = '',
    this.quality = 'auto',
    this.fps = 30,
    this.streamResolution = StreamResolution.native,
    this.maxVideoMbps,
    this.videoCodec = DesktopVideoCodec.auto,
    this.softwareEncoding = false,
    this.permissions = const DesktopPermissions(),
  });

  factory DesktopSettings.fromJson(Map<String, Object?> json) {
    final video = json['video'] as Map?;
    final encoding = video?['encoding'] as Map?;
    final quality = VideoQuality.fromJson({
      'preset': json['quality'],
      'fps': json['fps'],
      'resolution': video?['resolution'],
      'max_mbps': video?['max_mbps'],
    });
    return DesktopSettings(
      useSelfHosted: json['use_self_hosted'] == true,
      directory: json['directory'] as String,
      forceRelay: json['force_relay'] == true,
      lanAllowed: json['lan_allowed'] == true,
      address: json['address'] as String,
      port: json['port'] as int,
      source: json['source'] as String,
      shareRoot: json['share_root'] as String,
      allowShell: json['allow_shell'] == true,
      approvalPolicy: ApprovalPolicy.fromValue(
        json['approval_policy'] ??
            (json['unattended'] == true
                ? ApprovalPolicy.trustedDevices.value
                : null),
      ),
      downloadRoot: json['download_root'] as String,
      quality: quality.profile.name,
      fps: quality.fps,
      streamResolution: quality.resolution,
      maxVideoMbps: quality.maxMbps,
      videoCodec: DesktopVideoCodec.parse(encoding?['codec']),
      softwareEncoding: encoding?['software'] == true,
      permissions: DesktopPermissions.fromJson(json['permissions']),
    );
  }

  final bool useSelfHosted,
      forceRelay,
      lanAllowed,
      allowShell,
      softwareEncoding;
  final ApprovalPolicy approvalPolicy;
  final String directory, address, source, shareRoot, downloadRoot, quality;
  final int port, fps;
  final StreamResolution streamResolution;
  final DesktopVideoCodec videoCodec;
  final int? maxVideoMbps;
  final DesktopPermissions permissions;

  VideoQuality get videoQuality => VideoQuality(
    profile: VideoProfile.parse(quality),
    fps: fps,
    resolution: streamResolution,
    maxMbps: maxVideoMbps,
  );

  String? activeDirectory(String defaultDirectory) =>
      normalizeDirectoryOrigin(useSelfHosted ? directory : defaultDirectory);

  DesktopSettings withLanListener({String? address, int? port}) =>
      DesktopSettings(
        useSelfHosted: useSelfHosted,
        directory: directory,
        forceRelay: forceRelay,
        lanAllowed: lanAllowed,
        address: address ?? this.address,
        port: port ?? this.port,
        source: source,
        shareRoot: shareRoot,
        allowShell: allowShell,
        approvalPolicy: approvalPolicy,
        downloadRoot: downloadRoot,
        quality: quality,
        fps: fps,
        streamResolution: streamResolution,
        maxVideoMbps: maxVideoMbps,
        videoCodec: videoCodec,
        softwareEncoding: softwareEncoding,
        permissions: permissions,
      );

  Map<String, Object?> toJson() => {
    'use_self_hosted': useSelfHosted,
    'directory': directory,
    if (forceRelay) 'force_relay': true,
    'lan_allowed': lanAllowed,
    'address': address,
    'port': port,
    'source': source,
    'share_root': shareRoot,
    'allow_shell': allowShell,
    'approval_policy': approvalPolicy.value,
    'download_root': downloadRoot,
    'quality': quality,
    'fps': fps,
    if (streamResolution != StreamResolution.native ||
        maxVideoMbps != null ||
        videoCodec != DesktopVideoCodec.auto ||
        softwareEncoding)
      'video': {
        'resolution': streamResolution.value,
        'max_mbps': maxVideoMbps,
        'encoding': {'codec': videoCodec.value, 'software': softwareEncoding},
      },
    'permissions': permissions.toJson(),
  };
}

/// Keep the two adjacent port concepts separate at the connection boundary.
final class LanTarget {
  const LanTarget(this.address, this.port);

  factory LanTarget.fromInput(String input, int defaultPort) {
    final value = input.trim().replaceAll('：', ':');
    final bracketed = RegExp(r'^\[([^\]]+)\]\s*(?::(.*))?$').firstMatch(value);
    if (bracketed != null) {
      return LanTarget(
        bracketed[1]!.trim(),
        bracketed[2]?.trim() ?? defaultPort.toString(),
      );
    }
    final first = value.indexOf(':');
    if (first >= 0 && first == value.lastIndexOf(':')) {
      return LanTarget(
        value.substring(0, first).trim(),
        value.substring(first + 1).trim(),
      );
    }
    return LanTarget(value, defaultPort.toString());
  }

  final String address, port;

  String get endpoint =>
      address.contains(':') ? '[$address]:$port' : '$address:$port';
}
