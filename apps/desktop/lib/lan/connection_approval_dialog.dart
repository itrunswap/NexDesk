import 'package:flutter/material.dart';

import '../connection/session_permissions.dart';
import 'host_controller.dart';
import 'native_collaboration.dart' show CollaborationCapability;

class ConnectionApprovalDialog extends StatefulWidget {
  const ConnectionApprovalDialog({
    super.key,
    required this.request,
    required this.onDecision,
  });

  final HostRequest request;
  final void Function(String decision, int capabilities, bool remember)
  onDecision;

  @override
  State<ConnectionApprovalDialog> createState() =>
      _ConnectionApprovalDialogState();
}

class _ConnectionApprovalDialogState extends State<ConnectionApprovalDialog> {
  static const _shell = 64;
  bool _remember = false;

  @override
  Widget build(BuildContext context) {
    final request = widget.request;
    final decide = widget.onDecision;
    if (request.capabilities & _shell != 0) {
      return AlertDialog(
        title: const Text('允许远程 Shell？'),
        content: Text('${request.peer} 请求以当前系统账号运行命令。此授权仅用于本次连接。'),
        actions: [
          TextButton(
            onPressed: () => decide('deny', 0, false),
            child: const Text('拒绝'),
          ),
          FilledButton(
            onPressed: () => decide('view', _shell, false),
            child: const Text('允许 Shell'),
          ),
        ],
      );
    }
    final permissions = SessionPermissions(
      control: request.canControl,
      capabilities: request.capabilities & SessionPermissions.full.capabilities,
    );
    final audio =
        permissions.effectiveCapabilities & CollaborationCapability.audio.bit;
    final canLimit =
        request.canControl || permissions.effectiveCapabilities != audio;
    final scope = request.displays ? '本机显示器' : '当前共享画面';
    return AlertDialog(
      title: Text(request.canControl ? '允许远程控制？' : '允许远程观看？'),
      scrollable: true,
      content: SizedBox(
        width: 365,
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Text('${request.peer} 请求连接$scope。'),
            const SizedBox(height: 8),
            Text(
              permissions.summary,
              style: Theme.of(context).textTheme.bodySmall,
            ),
            if (request.controller != null)
              CheckboxListTile(
                contentPadding: EdgeInsets.zero,
                controlAffinity: ListTileControlAffinity.leading,
                title: const Text('信任此设备 30 天'),
                subtitle: const Text('下次按本次批准的权限免确认。'),
                value: _remember,
                onChanged: (value) => setState(() => _remember = value == true),
              ),
          ],
        ),
      ),
      actions: [
        TextButton(
          onPressed: () => decide('deny', 0, false),
          child: const Text('拒绝'),
        ),
        if (canLimit)
          OutlinedButton(
            onPressed: () => decide('view', audio, _remember),
            child: const Text('仅观看'),
          ),
        FilledButton(
          onPressed: () => decide(
            request.canControl ? 'control' : 'view',
            permissions.effectiveCapabilities,
            _remember,
          ),
          child: Text(
            request.canControl
                ? '允许控制'
                : canLimit
                ? '允许连接'
                : '允许观看',
          ),
        ),
      ],
    );
  }
}
