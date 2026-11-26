import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../lan/host_controller.dart';
import 'home_panel.dart';

class LocalDevice extends StatefulWidget {
  const LocalDevice({super.key, required this.host, required this.lan});

  final HostController? host;
  final bool lan;

  @override
  State<LocalDevice> createState() => _LocalDeviceState();
}

class _LocalDeviceState extends State<LocalDevice> {
  final _port = TextEditingController();
  final _portFocus = FocusNode();
  bool _showPassword = true;
  bool _changingPassword = false;
  bool _editingPort = false;
  String? _portIssue;

  @override
  void dispose() {
    _port.dispose();
    _portFocus.dispose();
    super.dispose();
  }

  void _notice(String message) {
    if (!mounted) return;
    ScaffoldMessenger.of(context)
      ..hideCurrentSnackBar()
      ..showSnackBar(SnackBar(content: Text(message)));
  }

  Future<void> _copy(String value, String message) async {
    try {
      await Clipboard.setData(ClipboardData(text: value));
      _notice(message);
    } on PlatformException {
      _notice('无法写入系统剪贴板。');
    }
  }

  Future<void> _changePassword(
    HostController host, {
    required bool custom,
    required String current,
  }) async {
    setState(() => _changingPassword = true);
    try {
      final choice = await showDialog<String>(
        context: context,
        builder: (_) => _PasswordChangeDialog(custom: custom),
      );
      if (!mounted || choice == null || choice == current) return;
      final problem = await host.changePassword(
        directory: !widget.lan,
        password: custom ? choice : null,
      );
      _notice(problem ?? '连接密码已更新');
    } finally {
      if (mounted) setState(() => _changingPassword = false);
    }
  }

  void _editPort(int current) {
    _port.text = current.toString();
    _port.selection = TextSelection(
      baseOffset: 0,
      extentOffset: _port.text.length,
    );
    setState(() {
      _editingPort = true;
      _portIssue = null;
    });
    _portFocus.requestFocus();
  }

  void _cancelPort() => setState(() {
    _editingPort = false;
    _portIssue = null;
  });

  Future<void> _savePort(HostController host) async {
    if (host.applying) return;
    final value = int.tryParse(_port.text);
    if (value == null || value < 1 || value > 65535) {
      setState(() => _portIssue = '请输入 1–65535 之间的端口。');
      _portFocus.requestFocus();
      return;
    }
    final config = host.settingsDraft;
    final problem = value == config.port
        ? null
        : await host.saveSettings(
            config.withLanListener(port: value),
            afterSession: true,
          );
    if (!mounted) return;
    if (problem == null) {
      _cancelPort();
    } else {
      setState(() => _portIssue = problem);
      _portFocus.requestFocus();
    }
  }

  Future<void> _chooseIp(HostController host, String address) async {
    final config = host.settingsDraft;
    if (address == config.address) return;
    final problem = await host.saveSettings(
      config.withLanListener(address: address),
      afterSession: true,
    );
    if (problem != null) _notice(problem);
  }

  Widget _lanOptions(HostController host) {
    final config = host.settingsDraft;
    final enabled = config.lanAllowed && !host.loading && !host.applying;
    return Wrap(
      crossAxisAlignment: WrapCrossAlignment.center,
      spacing: 8,
      children: [
        PopupMenuButton<String>(
          enabled: enabled,
          tooltip: '从本机网络接口选择 IP',
          position: PopupMenuPosition.under,
          constraints: const BoxConstraints(
            minWidth: 160,
            maxWidth: 280,
            maxHeight: 310,
          ),
          onSelected: (value) => _chooseIp(host, value),
          itemBuilder: (context) => [
            CheckedPopupMenuItem(
              value: '',
              checked: config.address.isEmpty,
              child: const Text('自动选择'),
            ),
            for (final ip in host.interfaces)
              CheckedPopupMenuItem(
                value: ip,
                checked: config.address == ip,
                child: Text(ip),
              ),
          ],
          child: Padding(
            padding: const EdgeInsets.symmetric(horizontal: 5, vertical: 6),
            child: Row(
              mainAxisSize: MainAxisSize.min,
              children: [
                Text(
                  '切换 IP',
                  style: TextStyle(
                    color: enabled
                        ? Theme.of(context).colorScheme.primary
                        : Theme.of(context).disabledColor,
                  ),
                ),
                Icon(
                  Icons.expand_more_rounded,
                  size: 15,
                  color: enabled
                      ? Theme.of(context).colorScheme.primary
                      : Theme.of(context).disabledColor,
                ),
              ],
            ),
          ),
        ),
        _textAction(
          '修改端口',
          enabled && !_editingPort ? () => _editPort(config.port) : null,
        ),
      ],
    );
  }

  Widget _textAction(String label, VoidCallback? action) => TextButton(
    style: TextButton.styleFrom(
      minimumSize: const Size(0, 29),
      visualDensity: VisualDensity.compact,
      padding: const EdgeInsets.symmetric(horizontal: 6),
      tapTargetSize: MaterialTapTargetSize.shrinkWrap,
      textStyle: Theme.of(context).textTheme.labelMedium,
    ),
    onPressed: action,
    child: Text(label),
  );

  Widget _portEditor(HostController host, String? endpoint) {
    final separator = endpoint?.lastIndexOf(':') ?? -1;
    final address = separator >= 0
        ? endpoint!.substring(0, separator)
        : endpoint;
    return _DeviceValue(
      leading: Padding(
        padding: const EdgeInsets.only(left: 10, right: 4),
        child: Row(
          children: [
            Expanded(
              child: Text(
                address ?? '—',
                overflow: TextOverflow.ellipsis,
                maxLines: 1,
              ),
            ),
            const Padding(padding: EdgeInsets.only(right: 5), child: Text(':')),
            SizedBox(
              width: 66,
              child: CallbackShortcuts(
                bindings: {
                  const SingleActivator(LogicalKeyboardKey.escape): _cancelPort,
                },
                child: TextField(
                  key: const ValueKey('local-port'),
                  controller: _port,
                  focusNode: _portFocus,
                  enabled: !host.applying,
                  keyboardType: TextInputType.number,
                  textInputAction: TextInputAction.done,
                  inputFormatters: [
                    FilteringTextInputFormatter.digitsOnly,
                    LengthLimitingTextInputFormatter(5),
                  ],
                  onSubmitted: (_) => _savePort(host),
                  onChanged: (_) {
                    if (_portIssue != null) setState(() => _portIssue = null);
                  },
                  decoration: const InputDecoration(
                    isDense: true,
                    contentPadding: EdgeInsets.symmetric(
                      horizontal: 5,
                      vertical: 6,
                    ),
                    hintText: '端口',
                  ),
                ),
              ),
            ),
          ],
        ),
      ),
      actions: [
        _icon(
          Icons.check_rounded,
          '保存端口',
          host.applying ? null : () => _savePort(host),
        ),
        _icon(
          Icons.close_rounded,
          '取消修改端口',
          host.applying ? null : _cancelPort,
        ),
      ],
    );
  }

  Widget _icon(IconData icon, String label, VoidCallback? action) => IconButton(
    tooltip: label,
    style: IconButton.styleFrom(
      minimumSize: const Size(34, 40),
      maximumSize: const Size(34, 40),
      padding: EdgeInsets.zero,
      tapTargetSize: MaterialTapTargetSize.shrinkWrap,
      foregroundColor: Theme.of(context).colorScheme.onSurfaceVariant,
      shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(6)),
    ),
    onPressed: action,
    icon: Icon(icon, size: 17),
  );

  Widget _value(
    String value, {
    String? copyDescription,
    VoidCallback? onCopy,
    bool id = false,
    bool sensitive = false,
  }) {
    final text = Theme.of(context).textTheme.titleMedium;
    final label = Padding(
      padding: const EdgeInsets.fromLTRB(11, 6, 3, 6),
      child: Text(
        value,
        maxLines: 1,
        overflow: TextOverflow.ellipsis,
        style: text?.copyWith(
          fontSize: id ? 21 : 16,
          letterSpacing: sensitive
              ? 1.2
              : id
              ? .6
              : 0,
          fontFeatures: const [FontFeature.tabularFigures()],
        ),
      ),
    );
    if (onCopy == null) return label;
    return Tooltip(
      message: copyDescription!,
      child: Semantics(
        label: copyDescription,
        button: true,
        excludeSemantics: sensitive,
        onTap: sensitive ? onCopy : null,
        child: InkWell(
          onTap: onCopy,
          child: Align(alignment: Alignment.centerLeft, child: label),
        ),
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    final host = widget.host;
    final lan = widget.lan;
    final address = lan
        ? host?.endpoint
        : host?.directoryStatus == 'revoked'
        ? null
        : host?.directoryDevice;
    final addressLabel = lan ? 'IP 和端口' : '设备 ID';
    final code = lan
        ? (host?.settings.lanAllowed == true ? host?.code : null)
        : (host?.directoryOrigin != null ? host?.directoryCode : null);
    final issue =
        host?.issue ?? (lan ? host?.lanIssue : null) ?? host?.shareIssue;
    final passwordAvailable =
        host != null && code != null && !_changingPassword;
    final shownAddress = !lan && address != null && address.length == 9
        ? '${address.substring(0, 3)} ${address.substring(3, 6)} ${address.substring(6)}'
        : address;
    final text = Theme.of(context).textTheme;
    return HomePanel(
      key: const ValueKey('local-device-panel'),
      title: '本机设备',
      subtitle: lan ? '分享本机地址和密码，接收局域网连接。' : '分享设备 ID 和密码，接收远程连接。',
      children: [
        Wrap(
          crossAxisAlignment: WrapCrossAlignment.center,
          spacing: 6,
          children: [
            Text(lan ? '本机 IP / 端口' : '本机设备 ID', style: text.labelMedium),
            if (lan && host != null) _lanOptions(host),
          ],
        ),
        const SizedBox(height: 5),
        if (_editingPort && host != null)
          _portEditor(host, address)
        else
          _DeviceValue(
            leading: _value(
              shownAddress ?? '—',
              id: !lan,
              copyDescription: '点击复制$addressLabel',
              onCopy: address == null
                  ? null
                  : () => _copy(address, '已复制$addressLabel'),
            ),
            actions: [
              _icon(
                Icons.content_copy_rounded,
                '复制$addressLabel',
                address == null
                    ? null
                    : () => _copy(address, '已复制$addressLabel'),
              ),
            ],
          ),
        if (_portIssue case final error?)
          Padding(
            padding: const EdgeInsets.only(top: 4),
            child: Text(
              error,
              style: text.bodySmall?.copyWith(
                color: Theme.of(context).colorScheme.error,
              ),
            ),
          ),
        const SizedBox(height: 9),
        Text('连接密码', style: text.labelMedium),
        const SizedBox(height: 5),
        _DeviceValue(
          leading: _value(
            code == null ? '—' : (_showPassword ? code : '••••••'),
            sensitive: true,
            copyDescription: '点击复制连接密码',
            onCopy: code == null ? null : () => _copy(code, '已复制连接密码'),
          ),
          actions: [
            _icon(
              _showPassword
                  ? Icons.visibility_off_outlined
                  : Icons.visibility_outlined,
              _showPassword ? '隐藏密码' : '显示密码',
              code == null
                  ? null
                  : () => setState(() => _showPassword = !_showPassword),
            ),
            _icon(
              Icons.refresh_rounded,
              '生成随机密码',
              passwordAvailable
                  ? () => _changePassword(host, custom: false, current: code)
                  : null,
            ),
            _icon(
              Icons.edit_outlined,
              '修改密码',
              passwordAvailable
                  ? () => _changePassword(host, custom: true, current: code)
                  : null,
            ),
          ],
        ),
        if (issue != null) Text(issue, style: text.bodySmall),
      ],
    );
  }
}

class _DeviceValue extends StatelessWidget {
  const _DeviceValue({required this.leading, required this.actions});

  final Widget leading;
  final List<Widget> actions;

  @override
  Widget build(BuildContext context) {
    final colors = Theme.of(context).colorScheme;
    return Material(
      color: colors.surfaceContainer,
      shape: RoundedRectangleBorder(
        side: BorderSide(color: colors.outlineVariant),
        borderRadius: BorderRadius.circular(7),
      ),
      clipBehavior: Clip.antiAlias,
      child: ConstrainedBox(
        constraints: const BoxConstraints(minHeight: 43),
        child: Row(
          children: [
            Expanded(child: leading),
            ...actions,
            const SizedBox(width: 2),
          ],
        ),
      ),
    );
  }
}

class _PasswordChangeDialog extends StatefulWidget {
  const _PasswordChangeDialog({required this.custom});

  final bool custom;

  @override
  State<_PasswordChangeDialog> createState() => _PasswordChangeDialogState();
}

class _PasswordChangeDialogState extends State<_PasswordChangeDialog> {
  final _form = GlobalKey<FormState>();
  final _password = TextEditingController();

  @override
  void dispose() {
    _password.dispose();
    super.dispose();
  }

  void _confirm() {
    if (!widget.custom || _form.currentState!.validate()) {
      Navigator.pop(context, _password.text);
    }
  }

  @override
  Widget build(BuildContext context) => AlertDialog(
    title: Text(widget.custom ? '修改连接密码' : '生成随机密码'),
    content: SizedBox(
      width: 290,
      child: Form(
        key: _form,
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            if (widget.custom) ...[
              TextFormField(
                controller: _password,
                autofocus: true,
                autocorrect: false,
                enableSuggestions: false,
                obscureText: true,
                keyboardType: TextInputType.visiblePassword,
                textInputAction: TextInputAction.done,
                decoration: const InputDecoration(
                  labelText: '新密码',
                  hintText: '六位英文字母或数字',
                ),
                inputFormatters: [
                  FilteringTextInputFormatter.allow(RegExp(r'[a-zA-Z0-9]')),
                  LengthLimitingTextInputFormatter(6),
                ],
                validator: (value) =>
                    value?.length == 6 ? null : '请输入六位英文字母或数字。',
                onFieldSubmitted: (_) => _confirm(),
              ),
              const SizedBox(height: 10),
            ],
            const Text('更改后，当前连接模式的旧密码失效，已有会话将断开。'),
          ],
        ),
      ),
    ),
    actions: [
      OutlinedButton(
        onPressed: () => Navigator.pop(context),
        child: const Text('取消'),
      ),
      FilledButton(onPressed: _confirm, child: const Text('确认')),
    ],
  );
}
