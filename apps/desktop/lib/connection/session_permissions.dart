import '../lan/native_collaboration.dart' show CollaborationCapability;

final class SessionPermissions {
  const SessionPermissions({required this.control, required this.capabilities});

  static const viewing = SessionPermissions(control: false, capabilities: 1);
  static const everyday = SessionPermissions(control: true, capabilities: 35);
  static const full = SessionPermissions(control: true, capabilities: 63);

  final bool control;
  final int capabilities;

  int get effectiveCapabilities =>
      control ? capabilities : capabilities & ~CollaborationCapability.text.bit;

  String get summary {
    final labels = [
      if (control) '键鼠控制',
      for (final capability in CollaborationCapability.values)
        if (effectiveCapabilities & capability.bit != 0) capability.label,
    ];
    return labels.isEmpty ? '仅观看画面' : labels.join('、');
  }

  bool permits(SessionPermissions previous) =>
      (control || !previous.control) &&
      (effectiveCapabilities & previous.effectiveCapabilities) ==
          previous.effectiveCapabilities;
}

final class DesktopPermissions {
  const DesktopPermissions({
    this.outgoing = SessionPermissions.everyday,
    this.incoming = SessionPermissions.full,
    this.deviceAuthorization = false,
  });

  factory DesktopPermissions.fromJson(Object? value) {
    if (value is! Map) return const DesktopPermissions();
    return DesktopPermissions(
      outgoing: SessionPermissions(
        control: value['request_control'] as bool? ?? true,
        capabilities:
            value['request_capabilities'] as int? ??
            SessionPermissions.everyday.capabilities,
      ),
      incoming: SessionPermissions(
        control: value['allow_control'] as bool? ?? true,
        capabilities:
            value['allowed_capabilities'] as int? ??
            SessionPermissions.full.capabilities,
      ),
      deviceAuthorization: value['device_authorization'] == true,
    );
  }

  final SessionPermissions outgoing, incoming;
  final bool deviceAuthorization;

  Map<String, Object?> toJson() => {
    'request_control': outgoing.control,
    'request_capabilities': outgoing.effectiveCapabilities,
    'allow_control': incoming.control,
    'allowed_capabilities': incoming.effectiveCapabilities,
    'device_authorization': deviceAuthorization,
  };
}
