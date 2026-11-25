import 'dart:convert';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../lan/host_controller.dart';

class DesktopMaintenance extends StatefulWidget {
  const DesktopMaintenance({super.key, required this.host});

  final HostController host;

  @override
  State<DesktopMaintenance> createState() => _DesktopMaintenanceState();
}

class _DesktopMaintenanceState extends State<DesktopMaintenance> {
  static const _native = MethodChannel('dev.nexdesk/background');
  Map<String, Object?> _status = const {};
  bool _working = false;
  String? _error;

  @override
  void initState() {
    super.initState();
    _refresh();
  }

  Future<void> _refresh() async {
    try {
      final status = await _native.invokeMapMethod<String, Object?>('status');
      if (mounted) setState(() => _status = status ?? const {});
    } on MissingPluginException {
      if (mounted) setState(() => _status = const {});
    } on PlatformException {
      if (mounted) setState(() => _error = '无法读取后台运行设置。');
    }
  }

  Future<void> _change(String method, bool enabled) async {
    setState(() {
      _working = true;
      _error = null;
    });
    try {
      await _native.invokeMethod<void>(method, enabled);
      await _refresh();
      if (mounted &&
          method == 'setTray' &&
          enabled &&
          _status['trayVisible'] == false) {
        setState(() => _error = '托盘图标暂不可见，关闭窗口时仍会退出。');
      }
    } on PlatformException {
      if (mounted) setState(() => _error = '系统未能应用此设置。');
    } finally {
      if (mounted) setState(() => _working = false);
    }
  }

  Future<void> _diagnostics() async {
    try {
      final events = await widget.host.native.local({
        'operation': 'session_history',
      });
      if (!mounted) return;
      final report = const JsonEncoder.withIndent('  ').convert({
        'app': 'NexDesk',
        'release': const String.fromEnvironment(
          'NEXDESK_RELEASE_VERSION',
          defaultValue: '0.1.0',
        ),
        'platform': Platform.operatingSystem,
        'directory_protocol': 1,
        'local_receiver': widget.host.endpoint == null
            ? 'unavailable'
            : 'ready',
        'directory': widget.host.directoryStatus ?? 'unconfigured',
        'session_events': events,
      });
      await showDialog<void>(
        context: context,
        builder: (dialog) => AlertDialog(
          title: const Text('本机诊断'),
          scrollable: true,
          content: SizedBox(width: 450, child: SelectableText(report)),
          actions: [
            TextButton(
              onPressed: () => Navigator.pop(dialog),
              child: const Text('关闭'),
            ),
            FilledButton(
              onPressed: () async {
                await Clipboard.setData(ClipboardData(text: report));
                if (dialog.mounted) Navigator.pop(dialog);
              },
              child: const Text('复制诊断'),
            ),
          ],
        ),
      );
    } on StateError {
      if (mounted) setState(() => _error = '无法读取本机诊断。');
    }
  }

  Future<void> _clearHistory() async {
    final clear = await showDialog<bool>(
      context: context,
      builder: (dialog) => AlertDialog(
        title: const Text('清除本机会话记录？'),
        content: const Text('这会删除诊断中的连接和断开记录。'),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(dialog, false),
            child: const Text('取消'),
          ),
          FilledButton(
            onPressed: () => Navigator.pop(dialog, true),
            child: const Text('清除记录'),
          ),
        ],
      ),
    );
    if (!mounted || clear != true) return;
    setState(() => _working = true);
    try {
      await widget.host.native.local({'operation': 'clear_session_history'});
      if (mounted) {
        setState(() => _error = null);
        ScaffoldMessenger.of(context)
            .showSnackBar(const SnackBar(content: Text('已清除本机会话记录。')));
      }
    } on StateError {
      if (mounted) setState(() => _error = '无法清除本机会话记录。');
    } finally {
      if (mounted) setState(() => _working = false);
    }
  }

  @override
  Widget build(BuildContext context) => ListView(
    padding: const EdgeInsets.all(20),
    children: [
      const Text('启动与后台'),
      SwitchListTile(
        contentPadding: EdgeInsets.zero,
        title: const Text('登录系统后启动 NexDesk'),
        value: _status['startupEnabled'] == true,
        onChanged: _status['startupSupported'] == true && !_working
            ? (enabled) => _change('setStartup', enabled)
            : null,
      ),
      SwitchListTile(
        contentPadding: EdgeInsets.zero,
        title: const Text('关闭窗口后保留在系统托盘'),
        subtitle: const Text('可在托盘重新打开窗口或退出应用。'),
        value: _status['trayEnabled'] == true,
        onChanged: _status['traySupported'] == true && !_working
            ? (enabled) => _change('setTray', enabled)
            : null,
      ),
      const SizedBox(height: 12),
      ListTile(
        contentPadding: EdgeInsets.zero,
        leading: const Icon(Icons.monitor_heart_outlined),
        title: const Text('查看和复制诊断'),
        onTap: _working ? null : _diagnostics,
      ),
      ListTile(
        contentPadding: EdgeInsets.zero,
        leading: const Icon(Icons.delete_outline),
        title: const Text('清除本机会话记录'),
        onTap: _working ? null : _clearHistory,
      ),
      if (_error case final error?)
        Semantics(liveRegion: true, child: Text(error)),
    ],
  );
}

class TrustedDevicesDialog extends StatefulWidget {
  const TrustedDevicesDialog({super.key, required this.host});

  final HostController host;

  @override
  State<TrustedDevicesDialog> createState() => _TrustedDevicesDialogState();
}

class _TrustedDevicesDialogState extends State<TrustedDevicesDialog> {
  late Future<List<Map<String, Object?>>> _devices = widget.host
      .trustedDevices();
  String? _error;
  bool _working = false;

  Future<void> _revoke(String scope, String identity) async {
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (dialog) => AlertDialog(
        title: const Text('撤销设备授权？'),
        content: const Text('该设备下次连接需重新批准，此模式中的接收会话将结束。'),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(dialog),
            child: const Text('取消'),
          ),
          FilledButton(
            onPressed: () => Navigator.pop(dialog, true),
            child: const Text('撤销授权'),
          ),
        ],
      ),
    );
    if (!mounted || confirmed != true) return;
    setState(() => _working = true);
    final revoked = await widget.host.revokeTrustedDevice(scope, identity);
    if (!mounted) return;
    setState(() {
      _working = false;
      _error = revoked ? null : '无法撤销该授权。';
      if (revoked) _devices = widget.host.trustedDevices();
    });
  }

  @override
  Widget build(BuildContext context) => AlertDialog(
    title: const Text('已授权设备'),
    scrollable: true,
    content: SizedBox(
      width: 490,
      child: FutureBuilder(
        future: _devices,
        builder: (context, snapshot) {
          final devices = snapshot.data;
          if (snapshot.hasError) return const Text('无法读取设备授权。');
          if (devices == null) return const LinearProgressIndicator();
          if (devices.isEmpty) return const Text('暂无已授权设备。');
          return Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              for (final device in devices)
                ListTile(
                  contentPadding: EdgeInsets.zero,
                  title: Text((device['identity'] as String).substring(0, 12)),
                  subtitle: Text(_description(device)),
                  trailing: TextButton(
                    onPressed: _working
                        ? null
                        : () => _revoke(
                            device['scope'] as String,
                            device['identity'] as String,
                          ),
                    child: const Text('撤销'),
                  ),
                ),
              if (_error case final error?) Text(error),
            ],
          );
        },
      ),
    ),
    actions: [
      TextButton(
        onPressed: () => Navigator.pop(context),
        child: const Text('关闭'),
      ),
    ],
  );

  static String _description(Map<String, Object?> device) {
    final scope = device['scope'] == 'lan' ? 'IP 直连' : device['scope'];
    final grant = device['input'] == true ? '可控制' : '仅观看';
    final expires = DateTime.fromMillisecondsSinceEpoch(
      (device['expires_at'] as int) * 1000,
    );
    final date = '${expires.year}-${expires.month}-${expires.day}';
    return '$scope · $grant · $date 到期';
  }
}
