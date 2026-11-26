import 'directory_origin.dart';
import 'session_permissions.dart';

enum ApprovalPolicy {
  manual('manual', '每次确认', '每次连接由本机批准。'),
  trustedDevices('trusted_devices', '可信设备免确认', '可信设备可在 30 天内按已批准的权限接入。'),
  password('password', '密码验证后连接', '通过连接密码验证后，按本机允许的权限接入。');

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
    this.permissions = const DesktopPermissions(),
  });

  factory DesktopSettings.fromJson(Map<String, Object?> json) =>
      DesktopSettings(
        useSelfHosted: json['use_self_hosted'] == true,
        directory: json['directory'] as String,
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
        quality: json['quality'] as String,
        fps: json['fps'] as int,
        permissions: DesktopPermissions.fromJson(json['permissions']),
      );

  final bool useSelfHosted, lanAllowed, allowShell;
  final ApprovalPolicy approvalPolicy;
  final String directory, address, source, shareRoot, downloadRoot, quality;
  final int port, fps;
  final DesktopPermissions permissions;

  String? activeDirectory(String defaultDirectory) =>
      normalizeDirectoryOrigin(useSelfHosted ? directory : defaultDirectory);

  DesktopSettings withLanListener({String? address, int? port}) =>
      DesktopSettings(
        useSelfHosted: useSelfHosted,
        directory: directory,
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
        permissions: permissions,
      );

  Map<String, Object?> toJson() => {
    'use_self_hosted': useSelfHosted,
    'directory': directory,
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
    'permissions': permissions.toJson(),
  };
}

/// Keep the two adjacent port concepts separate at the connection boundary.
final class LanTarget {
  const LanTarget(this.address, this.port);

  factory LanTarget.fromInput(String input, int defaultPort) {
    final value = input.trim();
    final bracketed = RegExp(r'^\[([^\]]+)\](?::(.*))?$').firstMatch(value);
    if (bracketed != null) {
      return LanTarget(bracketed[1]!, bracketed[2] ?? defaultPort.toString());
    }
    final first = value.indexOf(':');
    if (first >= 0 && first == value.lastIndexOf(':')) {
      return LanTarget(value.substring(0, first), value.substring(first + 1));
    }
    return LanTarget(value, defaultPort.toString());
  }

  final String address, port;

  String get endpoint =>
      address.contains(':') ? '[$address]:$port' : '$address:$port';
}
