import 'package:flutter/material.dart';

import '../lan/capability_choices.dart';
import '../lan/native_collaboration.dart' show CollaborationCapability;
import 'session_permissions.dart';

enum _Profile {
  viewing('仅观看', SessionPermissions.viewing),
  everyday('日常远控', SessionPermissions.everyday),
  full('完整远控', SessionPermissions.full),
  custom('自定义', null);

  const _Profile(this.label, this.permissions);

  final String label;
  final SessionPermissions? permissions;

  static _Profile forPermissions(SessionPermissions value) => values.firstWhere(
    (profile) =>
        profile.permissions?.control == value.control &&
        profile.permissions?.effectiveCapabilities ==
            value.effectiveCapabilities,
    orElse: () => custom,
  );
}

class PermissionProfilePicker extends StatefulWidget {
  const PermissionProfilePicker({
    super.key,
    required this.label,
    required this.value,
    required this.onChanged,
  });

  final String label;
  final SessionPermissions value;
  final ValueChanged<SessionPermissions> onChanged;

  @override
  State<PermissionProfilePicker> createState() =>
      _PermissionProfilePickerState();
}

class _PermissionProfilePickerState extends State<PermissionProfilePicker> {
  late _Profile _profile = _Profile.forPermissions(widget.value);

  @override
  Widget build(BuildContext context) {
    final value = widget.value;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        DropdownButtonFormField<_Profile>(
          initialValue: _profile,
          isExpanded: true,
          decoration: InputDecoration(labelText: widget.label),
          items: [
            for (final profile in _Profile.values)
              DropdownMenuItem(value: profile, child: Text(profile.label)),
          ],
          onChanged: (next) {
            if (next == null) return;
            setState(() => _profile = next);
            if (next.permissions case final permissions?) {
              widget.onChanged(permissions);
            }
          },
        ),
        const SizedBox(height: 6),
        Text(value.summary, style: Theme.of(context).textTheme.bodySmall),
        if (_profile == _Profile.custom) ...[
          SwitchListTile(
            contentPadding: EdgeInsets.zero,
            title: const Text('键鼠控制'),
            value: value.control,
            onChanged: (enabled) => widget.onChanged(
              SessionPermissions(
                control: enabled,
                capabilities: enabled
                    ? value.capabilities
                    : value.capabilities & ~CollaborationCapability.text.bit,
              ),
            ),
          ),
          CapabilityChoices(
            available: value.control
                ? SessionPermissions.full.capabilities
                : SessionPermissions.full.capabilities &
                      ~CollaborationCapability.text.bit,
            selected: value.effectiveCapabilities,
            onChanged: (bits) => widget.onChanged(
              SessionPermissions(control: value.control, capabilities: bits),
            ),
          ),
        ],
      ],
    );
  }
}
