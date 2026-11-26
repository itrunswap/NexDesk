import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../connection/session_permissions.dart';

final class ConnectionPassword {
  const ConnectionPassword({
    required this.code,
    required this.control,
    required this.remember,
    required this.stored,
    required this.capabilities,
    this.deviceAuthorization = false,
  });

  factory ConnectionPassword.request({
    required DesktopPermissions permissions,
    required bool offersCollaboration,
    String code = '',
    bool remember = false,
    bool stored = false,
  }) => ConnectionPassword(
    code: stored ? '' : code,
    control: permissions.outgoing.control,
    remember: remember || stored,
    stored: stored,
    capabilities: offersCollaboration
        ? permissions.outgoing.effectiveCapabilities
        : 0,
    deviceAuthorization: offersCollaboration && permissions.deviceAuthorization,
  );

  final String code;
  final bool control, remember, stored;
  final bool deviceAuthorization;
  final int capabilities;
}

Future<ConnectionPassword?> askConnectionPassword(
  BuildContext context, {
  required String target,
  required bool saved,
  required bool offersCollaboration,
  DesktopPermissions permissions = const DesktopPermissions(),
}) => showDialog<ConnectionPassword>(
  context: context,
  builder: (_) => _PasswordDialog(
    target: target,
    saved: saved,
    offersCollaboration: offersCollaboration,
    permissions: permissions,
  ),
);

class _PasswordDialog extends StatefulWidget {
  const _PasswordDialog({
    required this.target,
    required this.saved,
    required this.offersCollaboration,
    required this.permissions,
  });

  final String target;
  final bool saved, offersCollaboration;
  final DesktopPermissions permissions;

  @override
  State<_PasswordDialog> createState() => _PasswordDialogState();
}

class _PasswordDialogState extends State<_PasswordDialog> {
  final _code = TextEditingController();
  final _focus = FocusNode();
  late bool _stored = widget.saved;
  late bool _remember = widget.saved;
  bool get _valid => _stored || _code.text.length == 6;

  void _submit() {
    if (!_valid) return;
    Navigator.pop(
      context,
      ConnectionPassword.request(
        permissions: widget.permissions,
        offersCollaboration: widget.offersCollaboration,
        code: _code.text,
        remember: _remember,
        stored: _stored,
      ),
    );
  }

  @override
  void dispose() {
    _code.dispose();
    _focus.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => AlertDialog(
    title: const Text('输入连接密码'),
    scrollable: true,
    content: SizedBox(
      width: 360,
      child: Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Text(widget.target, style: Theme.of(context).textTheme.bodySmall),
          const SizedBox(height: 14),
          if (_stored)
            Wrap(
              spacing: 8,
              crossAxisAlignment: WrapCrossAlignment.center,
              children: [
                const Text('使用已保存的密码'),
                TextButton(
                  onPressed: () {
                    setState(() => _stored = false);
                    _focus.requestFocus();
                  },
                  child: const Text('重新输入'),
                ),
              ],
            ),
          if (!_stored) ...[
            TextField(
              key: const ValueKey('connection-password'),
              controller: _code,
              focusNode: _focus,
              autofocus: true,
              obscureText: true,
              autocorrect: false,
              enableSuggestions: false,
              inputFormatters: [
                FilteringTextInputFormatter.allow(RegExp('[a-zA-Z0-9]')),
                LengthLimitingTextInputFormatter(6),
              ],
              decoration: const InputDecoration(labelText: '六位连接密码'),
              onChanged: (_) => setState(() {}),
              onSubmitted: (_) => _submit(),
            ),
            CheckboxListTile(
              contentPadding: EdgeInsets.zero,
              controlAffinity: ListTileControlAffinity.leading,
              title: const Text('记住密码'),
              value: _remember,
              onChanged: (value) => setState(() => _remember = value == true),
            ),
            if (widget.saved)
              Align(
                alignment: Alignment.centerLeft,
                child: TextButton(
                  onPressed: () => setState(() => _stored = true),
                  child: const Text('改用已保存的密码'),
                ),
              ),
          ],
          Text(
            widget.permissions.outgoing.control ? '将请求远程控制' : '将请求观看桌面',
            style: Theme.of(context).textTheme.bodySmall,
          ),
        ],
      ),
    ),
    actions: [
      TextButton(
        onPressed: () => Navigator.pop(context),
        child: const Text('取消'),
      ),
      FilledButton(
        onPressed: _valid ? _submit : null,
        child: const Text('确认连接'),
      ),
    ],
  );
}
