import 'dart:async';

import 'package:flutter/material.dart';

import '../connection/desktop_settings.dart';
import 'connection_password.dart';
import 'native_desktop.dart';
import 'session_status.dart';
import 'session_window.dart';

class ConnectDialog extends StatefulWidget {
  const ConnectDialog({
    super.key,
    required this.native,
    required this.address,
    required this.port,
    required this.origin,
    required this.label,
    required this.password,
    required this.settings,
  });

  final NativeDesktop native;
  final String address, label;
  final int port;
  final String? origin;
  final ConnectionPassword password;
  final DesktopSettings settings;

  @override
  State<ConnectDialog> createState() => _ConnectDialogState();
}

class _ConnectDialogState extends State<ConnectDialog> {
  Timer? _timer;
  int _viewer = 0;
  int _status = 1;
  String? _fingerprint, _error;
  bool _opening = false;
  bool _retryPassword = false;

  @override
  void initState() {
    super.initState();
    _start(widget.password);
  }

  void _start(ConnectionPassword password) {
    final id = widget.native.connectTarget(
      widget.address,
      widget.port,
      widget.origin,
      password,
      forceRelay: widget.settings.forceRelay,
    );
    if (id == 0) {
      _fail('无法发起连接，请核对连接信息。');
      return;
    }
    setState(() {
      _viewer = id;
      _status = 1;
      _fingerprint = null;
      _error = null;
      _retryPassword = false;
    });
    _timer = Timer.periodic(
      const Duration(milliseconds: 100),
      (_) => _check(password.remember),
    );
  }

  void _check(bool remembered) {
    final frame = widget.native.preview.inspect(_viewer);
    final status = frame.status;
    final issue = sessionError(status, byId: widget.origin != null);
    if (issue != null) {
      _fail(issue, retryPassword: status == 4 || status == 14);
      return;
    }
    if (_status != status) {
      setState(() {
        _status = status;
        _fingerprint = status == 12
            ? widget.native.inspect(_viewer).identity
            : null;
      });
    }
    if (status == 2 && frame.width > 0 && frame.height > 0) {
      unawaited(_openWindow(remembered));
    }
  }

  Future<void> _openWindow(bool remembered) async {
    _timer?.cancel();
    final id = _viewer;
    setState(() => _opening = true);
    final launch = SessionWindowLaunch(
      viewer: id,
      address: widget.address,
      port: widget.port,
      origin: widget.origin,
      saved: remembered,
      settings: widget.settings,
    );
    final target = widget.label == launch.label
        ? launch.label
        : '${widget.label} · ${launch.label}';
    final opened = await SessionWindow.open(launch, 'NexDesk · $target');
    if (!mounted) return;
    if (opened) {
      // The session window is now responsible for releasing this viewer.
      _viewer = 0;
      final route = ModalRoute.of<int>(context)!;
      if (route.isCurrent) {
        Navigator.of(context).pop(id);
      } else {
        Navigator.of(context).removeRoute(route, id);
      }
    } else {
      _fail('无法打开独立的远程桌面窗口。请关闭此提示后重试。');
    }
  }

  void _fail(String issue, {bool retryPassword = false}) {
    _stop();
    setState(() {
      _error = issue;
      _opening = false;
      _retryPassword = retryPassword;
    });
  }

  void _stop() {
    _timer?.cancel();
    if (_viewer != 0) {
      unawaited(widget.native.preview.stop(_viewer));
      _viewer = 0;
    }
  }

  Future<void> _enterPassword() async {
    final password = await askConnectionPassword(
      context,
      target: widget.label,
      saved: false,
      offersCollaboration: widget.native.collaboration != null,
      permissions: widget.settings.permissions,
    );
    if (mounted && password != null) _start(password);
  }

  @override
  void dispose() {
    _stop();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => PopScope(
    canPop: !_opening,
    child: AlertDialog(
      title: Text(_error == null ? '正在连接' : '连接失败'),
      scrollable: true,
      content: SizedBox(
        width: 336,
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          mainAxisSize: MainAxisSize.min,
          children: [
            Text(widget.label, style: Theme.of(context).textTheme.labelLarge),
            const SizedBox(height: 10),
            Semantics(
              liveRegion: true,
              child: Text(_error ?? sessionProgress(_status)),
            ),
            if (_error == null && _status != 12) ...[
              const SizedBox(height: 14),
              const LinearProgressIndicator(),
            ],
            if (_fingerprint case final fingerprint? when _error == null) ...[
              const SizedBox(height: 12),
              const Text('请与被控电脑核对以下指纹。'),
              const SizedBox(height: 6),
              SelectableText(fingerprint),
            ],
          ],
        ),
      ),
      actions: [
        TextButton(
          onPressed: _opening ? null : () => Navigator.of(context).pop(),
          child: Text(_error == null ? '取消' : '关闭'),
        ),
        if (_retryPassword)
          FilledButton(onPressed: _enterPassword, child: const Text('重新输入密码')),
        if (_error == null && _fingerprint != null)
          FilledButton(
            onPressed: () => widget.native.trust(_viewer),
            child: const Text('指纹一致，继续连接'),
          ),
      ],
    ),
  );
}
