import 'dart:io';
import 'dart:math' as math;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../lan/host_controller.dart';
import '../preview/native_preview.dart';
import 'desktop_maintenance.dart';
import 'desktop_settings.dart';
import 'directory_origin.dart';
import 'permission_profile_picker.dart';
import 'session_permissions.dart';
import 'settings_section.dart';
import 'video_quality.dart';
import 'video_quality_fields.dart';

class SettingsDialog extends StatefulWidget {
  const SettingsDialog({super.key, required this.host, this.directory});

  final HostController host;
  final String? directory;

  @override
  State<SettingsDialog> createState() => _SettingsDialogState();
}

class _SettingsDialogState extends State<SettingsDialog> {
  late final DesktopSettings _initial = widget.host.settingsDraft;
  late final _historyOrigin = normalizeDirectoryOrigin(widget.directory ?? '');
  late final _defaultOrigin = normalizeDirectoryOrigin(
    widget.host.defaultDirectory,
  );
  late String _directory =
      _historyOrigin != null && _historyOrigin != _defaultOrigin
      ? _historyOrigin
      : _initial.directory;
  late final _lanPort = TextEditingController(text: _initial.port.toString());
  late final _shared = TextEditingController(text: _initial.shareRoot);
  late final _downloads = TextEditingController(text: _initial.downloadRoot);
  late bool _selfHosted = _historyOrigin == null
      ? _initial.useSelfHosted
      : _historyOrigin != _defaultOrigin;
  late bool _forceRelay = _initial.forceRelay;
  late bool _lan = _initial.lanAllowed, _shell = _initial.allowShell;
  late ApprovalPolicy _approvalPolicy = _initial.approvalPolicy;
  late bool _deviceAuthorization = _initial.permissions.deviceAuthorization;
  late SessionPermissions _outgoing = _initial.permissions.outgoing;
  late SessionPermissions _incoming = _initial.permissions.incoming;
  late String _address = _initial.address, _source = _initial.source;
  late VideoQuality _quality = _initial.videoQuality;
  late DesktopVideoCodec _codec = _initial.videoCodec;
  late bool _softwareEncoding = _initial.softwareEncoding;
  String? _error, _testStatus;
  bool _saving = false, _testing = false, _refreshing = false;

  String? get _testOrigin =>
      _selfHosted ? normalizeDirectoryOrigin(_directory) : _defaultOrigin;

  Future<void> _editDirectory({bool enable = false}) async {
    final result = await showDialog<String>(
      context: context,
      builder: (_) => _SelfHostedDialog(origin: _directory),
    );
    if (!mounted || result == null) return;
    setState(() {
      _directory = result;
      _selfHosted = enable || _selfHosted;
      _testStatus = null;
      _error = null;
    });
  }

  Future<void> _toggleSelfHosted(bool value) async {
    if (value && normalizeDirectoryOrigin(_directory) == null) {
      await _editDirectory(enable: true);
      return;
    }
    setState(() {
      _selfHosted = value;
      _testStatus = null;
    });
  }

  Future<void> _test() async {
    final origin = _testOrigin;
    if (origin == null) return;
    setState(() {
      _testing = true;
      _testStatus = null;
    });
    final result = await widget.host.checkDirectory(origin);
    if (!mounted) return;
    setState(() {
      _testing = false;
      _testStatus = origin == _testOrigin
          ? result ?? 'NexDesk 目录可达，协议版本兼容。'
          : null;
    });
  }

  Future<void> _save() async {
    if (_saving) return;
    final port = int.tryParse(_lanPort.text.trim());
    final validPort = port != null && port >= 1 && port <= 65535;
    final origin = normalizeDirectoryOrigin(_directory);
    if (_lan && !validPort) {
      setState(() => _error = '本机监听端口须为 1–65535。');
      return;
    }
    if (_selfHosted && origin == null) {
      setState(() => _error = '请填写有效的自托管地址和 HTTPS 端口。');
      return;
    }
    final value = DesktopSettings(
      useSelfHosted: _selfHosted,
      directory: origin ?? '',
      forceRelay: _forceRelay,
      lanAllowed: _lan,
      address: _address,
      port: validPort ? port : _initial.port,
      source: _source,
      shareRoot: _shared.text.trim(),
      allowShell: _shell,
      approvalPolicy: _approvalPolicy,
      downloadRoot: _downloads.text.trim(),
      quality: _quality.profile.name,
      fps: _quality.fps,
      streamResolution: _quality.resolution,
      maxVideoMbps: _quality.maxMbps,
      videoCodec: _codec,
      softwareEncoding: _softwareEncoding,
      permissions: DesktopPermissions(
        outgoing: _outgoing,
        incoming: _incoming,
        deviceAuthorization: _deviceAuthorization,
      ),
    );
    var afterSession = false;
    if (widget.host.affectsCurrentSession(value)) {
      final choice = await showDialog<bool>(
        context: context,
        builder: (context) => AlertDialog(
          title: const Text('何时应用接收设置？'),
          content: const Text('立即应用会结束受影响的接收会话。'),
          actions: [
            TextButton(
              onPressed: () => Navigator.pop(context),
              child: const Text('取消'),
            ),
            TextButton(
              onPressed: () => Navigator.pop(context, false),
              child: const Text('立即应用'),
            ),
            FilledButton(
              onPressed: () => Navigator.pop(context, true),
              child: const Text('会话结束后应用'),
            ),
          ],
        ),
      );
      if (!mounted || choice == null) return;
      afterSession = choice;
    }
    setState(() {
      _saving = true;
      _error = null;
    });
    final error = await widget.host.saveSettings(
      value,
      afterSession: afterSession,
    );
    if (!mounted) return;
    setState(() {
      _saving = false;
      _error = error;
    });
    if (error == null) Navigator.pop(context, true);
  }

  Future<void> _refreshDevices() async {
    setState(() => _refreshing = true);
    await widget.host.refreshDevices();
    if (mounted) setState(() => _refreshing = false);
  }

  Future<void> _pick(TextEditingController field) async {
    try {
      final path = await NativePreview.selectDirectory();
      if (mounted && path != null) field.text = path;
    } on PlatformException {
      if (mounted) setState(() => _error = '目录选取失败，可以手动输入完整路径。');
    } on MissingPluginException {
      if (mounted) setState(() => _error = '目录选取不可用，可以手动输入完整路径。');
    }
  }

  @override
  void dispose() {
    _lanPort.dispose();
    _shared.dispose();
    _downloads.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => PopScope(
    canPop: !_saving,
    child: Dialog(
      insetPadding: const EdgeInsets.all(16),
      child: SizedBox(
        width: 590,
        height: math.min(625, MediaQuery.sizeOf(context).height - 36),
        child: DefaultTabController(
          length: 4,
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              Padding(
                padding: const EdgeInsets.fromLTRB(22, 18, 22, 2),
                child: Text(
                  '设置',
                  style: Theme.of(context).textTheme.headlineSmall,
                ),
              ),
              const TabBar(
                labelPadding: EdgeInsets.symmetric(horizontal: 8),
                tabs: [
                  Tab(text: '常规'),
                  Tab(text: '权限'),
                  Tab(text: '共享'),
                  Tab(text: '维护'),
                ],
              ),
              Expanded(
                child: IgnorePointer(
                  ignoring: _saving,
                  child: TabBarView(
                    children: [
                      _page('general', [
                        _idService(),
                        _lanListener(),
                        _video(),
                        _storage(),
                      ]),
                      _page('permissions', _permissionSettings()),
                      _page('sharing', _sharing()),
                      DesktopMaintenance(host: widget.host),
                    ],
                  ),
                ),
              ),
              const Divider(height: 1),
              if (_error case final error?)
                Padding(
                  padding: const EdgeInsets.fromLTRB(20, 10, 20, 0),
                  child: Semantics(
                    liveRegion: true,
                    child: Text(
                      error,
                      style: TextStyle(
                        color: Theme.of(context).colorScheme.error,
                      ),
                    ),
                  ),
                ),
              Padding(
                padding: const EdgeInsets.fromLTRB(16, 12, 16, 16),
                child: OverflowBar(
                  alignment: MainAxisAlignment.end,
                  spacing: 8,
                  overflowSpacing: 8,
                  children: [
                    TextButton(
                      onPressed: _saving ? null : () => Navigator.pop(context),
                      child: const Text('取消'),
                    ),
                    FilledButton(
                      onPressed: _saving || _testing || _refreshing
                          ? null
                          : _save,
                      child: Text(_saving ? '正在保存…' : '保存'),
                    ),
                  ],
                ),
              ),
            ],
          ),
        ),
      ),
    ),
  );

  Widget _page(String key, List<Widget> children) => ListView(
    key: PageStorageKey(key),
    padding: const EdgeInsets.fromLTRB(18, 16, 18, 0),
    children: children,
  );

  Widget _idService() => SettingsSection(
    title: 'ID 服务',
    children: [
      SwitchListTile(
        contentPadding: EdgeInsets.zero,
        dense: true,
        title: const Text('使用自托管服务'),
        value: _selfHosted,
        onChanged: _toggleSelfHosted,
      ),
      const Divider(height: 1),
      const SizedBox(height: 9),
      _note(_testOrigin ?? (_selfHosted ? '尚未填写自托管地址' : '尚未配置默认 ID 服务')),
      const SizedBox(height: 8),
      Wrap(
        spacing: 8,
        runSpacing: 4,
        children: [
          OutlinedButton(
            onPressed: () => _editDirectory(),
            child: const Text('编辑自托管'),
          ),
          OutlinedButton(
            onPressed: _testing || _testOrigin == null ? null : _test,
            child: Text(_testing ? '正在测试…' : '测试连接'),
          ),
        ],
      ),
      if (_testStatus case final status?) ...[
        const SizedBox(height: 6),
        Semantics(liveRegion: true, child: _note(status)),
      ],
      const SizedBox(height: 8),
      const Divider(height: 1),
      SwitchListTile(
        contentPadding: EdgeInsets.zero,
        dense: true,
        title: const Text('仅通过 TURN 中继'),
        subtitle: const Text('用于本机新发起的 ID 连接。关闭时按 ID 服务配置自动选路。'),
        value: _forceRelay,
        onChanged: (value) => setState(() => _forceRelay = value),
      ),
    ],
  );

  Widget _lanListener() => SettingsSection(
    title: 'IP 直连',
    children: [
      SwitchListTile(
        contentPadding: EdgeInsets.zero,
        dense: true,
        title: const Text('允许局域网设备连接本机'),
        value: _lan,
        onChanged: (value) => setState(() => _lan = value),
      ),
      _choice(
        '监听网卡',
        _address,
        {
          '': '自动选择私网地址',
          for (final address in widget.host.interfaces) address: address,
          if (_address.isNotEmpty && !widget.host.interfaces.contains(_address))
            _address: '$_address（当前不可用）',
        },
        (value) => setState(() => _address = value),
        enabled: _lan,
      ),
      const SizedBox(height: 6),
      _note('自动模式选择一个可用的私网地址。'),
      const SizedBox(height: 12),
      _PortField(controller: _lanPort, label: '本机监听端口', enabled: _lan),
      Align(
        alignment: Alignment.centerLeft,
        child: TextButton(
          onPressed: _refreshing ? null : _refreshDevices,
          child: const Text('刷新网卡和显示器'),
        ),
      ),
    ],
  );

  List<Widget> _permissionSettings() => [
    SettingsSection(
      title: '作为主控端',
      children: [
        const SizedBox(height: 4),
        PermissionProfilePicker(
          label: '默认请求权限',
          value: _outgoing,
          onChanged: (value) => setState(() => _outgoing = value),
        ),
        const SizedBox(height: 8),
        const Divider(height: 1),
        SwitchListTile(
          contentPadding: EdgeInsets.zero,
          dense: true,
          title: const Text('使用本机设备授权'),
          subtitle: const Text('连接时提供本机身份，供远端匹配已授权的设备。'),
          value: _deviceAuthorization,
          onChanged: (value) => setState(() => _deviceAuthorization = value),
        ),
      ],
    ),
    SettingsSection(
      title: '作为被控端',
      children: [
        const SizedBox(height: 4),
        PermissionProfilePicker(
          label: '允许的权限上限',
          value: _incoming,
          onChanged: (value) => setState(() => _incoming = value),
        ),
        const SizedBox(height: 8),
        _note('来访只会获得双方都允许且本机支持的功能。'),
      ],
    ),
  ];

  List<Widget> _sharing() => [
    SettingsSection(
      title: '接入与可信设备',
      children: [
        const SizedBox(height: 4),
        _choice(
          '接入方式',
          _approvalPolicy.value,
          {
            for (final policy in ApprovalPolicy.values)
              policy.value: policy.label,
          },
          (value) =>
              setState(() => _approvalPolicy = ApprovalPolicy.fromValue(value)),
        ),
        const SizedBox(height: 8),
        _note(_approvalPolicy.description),
        if (_approvalPolicy == ApprovalPolicy.trustedOnly) ...[
          const SizedBox(height: 6),
          _note('添加新设备时，先使用「可信设备免确认」，在下一次来访时批准并保存设备授权。'),
        ],
        const SizedBox(height: 8),
        const Divider(height: 1),
        ListTile(
          contentPadding: EdgeInsets.zero,
          dense: true,
          title: const Text('管理可信设备'),
          trailing: const Icon(Icons.chevron_right),
          onTap: () => showDialog<void>(
            context: context,
            builder: (_) => TrustedDevicesDialog(host: widget.host),
          ),
        ),
      ],
    ),
    SettingsSection(
      title: '屏幕与文件共享',
      children: [
        const SizedBox(height: 4),
        _choice('初始共享来源', _source, {
          '': '自动选择显示器',
          for (final source in widget.host.sources) source.id: source.label,
          if (_source.isNotEmpty &&
              !widget.host.sources.any((source) => source.id == _source))
            _source: '之前的显示器暂不可用',
        }, (value) => setState(() => _source = value)),
        if (Platform.isMacOS)
          Align(
            alignment: Alignment.centerLeft,
            child: TextButton(
              onPressed: widget.host.requestSystemPermissions,
              child: const Text('申请系统录屏与辅助功能权限'),
            ),
          ),
        const SizedBox(height: 12),
        _folder(_shared, '被控端文件共享目录'),
        const SizedBox(height: 8),
        _note('远端在获得文件权限后可访问此目录。留空时关闭文件共享。'),
      ],
    ),
    SettingsSection(
      title: '高级权限',
      children: [
        SwitchListTile(
          contentPadding: EdgeInsets.zero,
          dense: true,
          title: const Text('允许在 ID 连接中请求远程 Shell'),
          subtitle: Text(
            _approvalPolicy == ApprovalPolicy.trustedOnly
                ? '仅允许已授权设备时，关闭远程 Shell 请求。'
                : '每次都在这台电脑单独批准。',
          ),
          value: _shell && _approvalPolicy != ApprovalPolicy.trustedOnly,
          onChanged: _approvalPolicy == ApprovalPolicy.trustedOnly
              ? null
              : (value) => setState(() => _shell = value),
        ),
      ],
    ),
  ];

  Widget _video() => SettingsSection(
    title: '画面',
    children: [
      const SizedBox(height: 6),
      _note('本机发起连接时的默认画质'),
      const SizedBox(height: 12),
      VideoQualityFields(
        value: _quality,
        onChanged: (value) => setState(() => _quality = value),
      ),
      const SizedBox(height: 12),
      const Divider(height: 1),
      const SizedBox(height: 12),
      _choice(
        '本机共享画面的编码格式',
        _codec.name,
        {for (final codec in DesktopVideoCodec.values) codec.name: codec.label},
        (value) =>
            setState(() => _codec = DesktopVideoCodec.values.byName(value)),
      ),
      const SizedBox(height: 6),
      _note('新来访会话会与对方协商可用的格式。'),
      SwitchListTile(
        contentPadding: EdgeInsets.zero,
        dense: true,
        title: const Text('优先硬件编码'),
        value: !_softwareEncoding,
        onChanged: (value) => setState(() => _softwareEncoding = !value),
      ),
    ],
  );

  Widget _storage() => SettingsSection(
    title: '本地存储',
    children: [
      const SizedBox(height: 4),
      _folder(_downloads, '本机文件接收目录'),
      const SizedBox(height: 8),
      _note('在文件管理中使用。留空时可在传输前选择目录。'),
    ],
  );

  Widget _note(String text) {
    final theme = Theme.of(context);
    return Text(
      text,
      style: theme.textTheme.bodySmall?.copyWith(
        color: theme.colorScheme.onSurfaceVariant,
      ),
    );
  }

  Widget _choice(
    String label,
    String value,
    Map<String, String> options,
    ValueChanged<String> onChanged, {
    bool enabled = true,
  }) => InputDecorator(
    decoration: InputDecoration(labelText: label, enabled: enabled),
    child: DropdownButtonHideUnderline(
      child: DropdownButton<String>(
        isExpanded: true,
        isDense: true,
        value: value,
        items: [
          for (final option in options.entries)
            DropdownMenuItem(
              value: option.key,
              child: Text(option.value, overflow: TextOverflow.ellipsis),
            ),
        ],
        onChanged: enabled
            ? (value) {
                if (value != null) onChanged(value);
              }
            : null,
      ),
    ),
  );

  Widget _folder(TextEditingController field, String label) => TextField(
    controller: field,
    decoration: InputDecoration(
      labelText: label,
      suffixIcon: IconButton(
        tooltip: '选择目录',
        icon: const Icon(Icons.folder_open_outlined),
        onPressed: () => _pick(field),
      ),
    ),
  );
}

class _SelfHostedDialog extends StatefulWidget {
  const _SelfHostedDialog({required this.origin});

  final String origin;

  @override
  State<_SelfHostedDialog> createState() => _SelfHostedDialogState();
}

class _SelfHostedDialogState extends State<_SelfHostedDialog> {
  late final _uri = Uri.tryParse(widget.origin);
  late final _server = TextEditingController(text: _editableAddress(_uri));
  late final _port = TextEditingController(
    text: _uri?.hasPort == true ? _uri!.port.toString() : '',
  );
  String? _error;

  static String _editableAddress(Uri? uri) {
    if (uri == null || uri.host.isEmpty) return '';
    final host = uri.host.contains(':') ? '[${uri.host}]' : uri.host;
    return uri.scheme == 'http' ? 'http://$host' : host;
  }

  void _confirm() {
    final origin = directoryOriginFromFields(_server.text, _port.text);
    if (origin == null) {
      setState(() => _error = '请输入有效的服务器地址和端口。');
      return;
    }
    Navigator.pop(context, origin);
  }

  void _clearError(String _) {
    if (_error != null) setState(() => _error = null);
  }

  @override
  void dispose() {
    _server.dispose();
    _port.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => AlertDialog(
    title: const Text('自托管 ID 服务'),
    scrollable: true,
    content: SizedBox(
      width: 360,
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        mainAxisSize: MainAxisSize.min,
        children: [
          TextField(
            key: const ValueKey('directory-server'),
            controller: _server,
            autofocus: true,
            autocorrect: false,
            textInputAction: TextInputAction.next,
            keyboardType: TextInputType.url,
            decoration: const InputDecoration(
              labelText: '服务器地址',
              hintText: 'desk.example.test',
            ),
            onChanged: _clearError,
          ),
          const SizedBox(height: 14),
          _PortField(
            controller: _port,
            label: '信令 HTTPS 端口',
            hint: '443',
            onChanged: _clearError,
            onSubmitted: (_) => _confirm(),
          ),
          if (_error case final error?) ...[
            const SizedBox(height: 8),
            Semantics(
              liveRegion: true,
              child: Text(
                error,
                style: TextStyle(color: Theme.of(context).colorScheme.error),
              ),
            ),
          ],
        ],
      ),
    ),
    actions: [
      TextButton(
        onPressed: () => Navigator.pop(context),
        child: const Text('取消'),
      ),
      FilledButton(onPressed: _confirm, child: const Text('确认')),
    ],
  );
}

class _PortField extends StatelessWidget {
  const _PortField({
    required this.controller,
    required this.label,
    this.hint,
    this.enabled = true,
    this.onChanged,
    this.onSubmitted,
  });

  final TextEditingController controller;
  final String label;
  final String? hint;
  final bool enabled;
  final ValueChanged<String>? onChanged, onSubmitted;

  @override
  Widget build(BuildContext context) => TextField(
    controller: controller,
    enabled: enabled,
    keyboardType: TextInputType.number,
    inputFormatters: [
      FilteringTextInputFormatter.digitsOnly,
      LengthLimitingTextInputFormatter(5),
    ],
    onChanged: onChanged,
    onSubmitted: onSubmitted,
    decoration: InputDecoration(labelText: label, hintText: hint),
  );
}
