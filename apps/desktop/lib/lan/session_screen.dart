import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../preview/native_preview.dart';
import 'connection_password.dart';
import 'floating_session_menu.dart';
import 'native_collaboration.dart';
import 'native_desktop.dart';
import 'remote_surface.dart';
import 'session_status.dart';
import 'session_tools.dart';
import 'session_window.dart';

class LanSessionScreen extends StatefulWidget {
  const LanSessionScreen({
    super.key,
    required this.native,
    required this.launch,
  });

  final NativeDesktop native;
  final SessionWindowLaunch launch;

  @override
  State<LanSessionScreen> createState() => _LanSessionScreenState();
}

class _LanSessionScreenState extends State<LanSessionScreen>
    with WidgetsBindingObserver {
  final _focus = FocusNode(debugLabel: 'remote desktop');
  int? _viewer, _texture;
  Timer? _poll;
  String? _issue;
  PreviewState _frames = const PreviewState();
  DesktopInfo _info = const DesktopInfo();
  late bool _canUseSaved = widget.launch.saved;
  bool _askingPassword = false;
  bool _qualityApplied = false;
  bool _switching = false;
  Timer? _switchTimeout;
  SurfaceScale _scale = SurfaceScale.fit;
  bool _full = false;
  bool _menuOpen = false;
  bool _controlPaused = false;
  bool _confirmingClose = false;
  bool _closing = false;

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    SessionWindow.onCloseRequested(_confirmClose);
    _viewer = widget.launch.viewer;
    _frames = const PreviewState(status: 1);
    unawaited(_attach(widget.launch.viewer));
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    if (state != AppLifecycleState.resumed) {
      _focus.unfocus();
      _release();
    }
  }

  Future<void> _reconnect() async {
    if (_viewer != null || _askingPassword) return;
    _askingPassword = true;
    final password = await askConnectionPassword(
      context,
      target: _targetLabel,
      saved: _canUseSaved,
      offersCollaboration: widget.native.collaboration != null,
      permissions: widget.launch.settings.permissions,
    );
    _askingPassword = false;
    if (!mounted || password == null) return;
    final id = widget.native.connectTarget(
      widget.launch.address,
      widget.launch.port,
      widget.launch.origin,
      password,
      forceRelay: widget.launch.settings.forceRelay,
    );
    if (id == 0) {
      setState(() => _issue = '请输入六位连接密码。');
      return;
    }
    setState(() {
      _viewer = id;
      _issue = null;
      _frames = const PreviewState(status: 1);
      _qualityApplied = false;
      _canUseSaved = password.remember;
    });
    await _attach(id);
  }

  Future<void> _attach(int id) async {
    try {
      final texture = await widget.native.preview.attachTexture(id);
      if (!mounted || _viewer != id) {
        await widget.native.preview.stop(id);
        return;
      }
      if (texture == null) {
        _stop('无法创建远控视频纹理。');
        return;
      }
      setState(() => _texture = texture);
      _poll = Timer.periodic(
        const Duration(milliseconds: 100),
        (_) => _refresh(id),
      );
    } on PlatformException {
      if (mounted && _viewer == id) _stop('无法创建远控视频纹理。');
    } on MissingPluginException {
      if (mounted && _viewer == id) _stop('当前桌面构建缺少视频纹理。');
    }
  }

  void _refresh(int id) {
    if (_viewer != id) return;
    final frames = widget.native.preview.inspect(id);
    final info = widget.native.inspect(id);
    final error = sessionError(
      frames.status,
      byId: widget.launch.origin != null,
    );
    if (error != null) {
      if (frames.status == 14 || frames.status == 4) _canUseSaved = false;
      _stop(error);
      return;
    }
    if (frames.status == 2 && !_qualityApplied) _applyDefaultQuality(id);
    if (_info.generation != info.generation ||
        _info.canInput && !info.canInput ||
        frames.status == 11) {
      _release();
    }
    setState(() {
      if (_info.generation != info.generation || frames.status == 11) {
        _switching = false;
      }
      _frames = frames;
      _info = info;
    });
  }

  void _applyDefaultQuality(int viewer) {
    final quality = widget.launch.settings.videoQuality;
    final native = widget.native.collaboration;
    if (native == null || quality.isDefault) {
      _qualityApplied = true;
      return;
    }
    if (native.inspect(viewer)['session'] is! Map) return;
    _qualityApplied = true;
    unawaited(
      native
          .setQuality(viewer, quality)
          .then<void>((applied) {
            if (mounted && _viewer == viewer && !applied.matches(quality)) {
              ScaffoldMessenger.of(
                context,
              ).showSnackBar(const SnackBar(content: Text('被控端只应用了部分画质设置。')));
            }
          })
          .onError<CollaborationIssue>((_, _) {
            if (mounted && _viewer == viewer) {
              ScaffoldMessenger.of(context).showSnackBar(
                const SnackBar(content: Text('远端未应用默认画质，可在会话中调整。')),
              );
            }
          }),
    );
  }

  void _release() {
    if (_viewer case final viewer?) {
      widget.native.send(viewer, DesktopAction.release);
    }
  }

  void _stop([String? issue]) {
    _poll?.cancel();
    _switchTimeout?.cancel();
    _release();
    if (_full && !_closing) unawaited(_fullscreen(false));
    if (_viewer case final viewer?) {
      unawaited(widget.native.preview.stop(viewer));
    }
    setState(() {
      _viewer = null;
      _texture = null;
      _frames = const PreviewState();
      _info = const DesktopInfo();
      _issue = issue;
      _switching = false;
      _menuOpen = false;
    });
  }

  Future<void> _confirmClose() async {
    if (_confirmingClose || _closing) return;
    _confirmingClose = true;
    _focus.unfocus();
    _release();
    try {
      if (_viewer != null) {
        final confirmed = await showDialog<bool>(
          context: context,
          builder: (context) => AlertDialog(
            title: const Text('断开远程连接？'),
            content: Text('将结束与 $_targetLabel 的当前会话。'),
            actions: [
              TextButton(
                onPressed: () => Navigator.pop(context, false),
                child: const Text('继续会话'),
              ),
              FilledButton(
                onPressed: () => Navigator.pop(context, true),
                child: const Text('断开连接'),
              ),
            ],
          ),
        );
        if (!mounted || confirmed != true) return;
      }
      _closing = true;
      _stop();
      if (!await SessionWindow.close() && mounted) {
        _closing = false;
        setState(() => _issue = '会话已结束，无法关闭此窗口，请使用系统关闭按钮。');
      }
    } finally {
      _confirmingClose = false;
    }
  }

  void _selectDisplay(String? display) {
    final viewer = _viewer;
    if (display == null ||
        viewer == null ||
        display == _info.selected ||
        _switching) {
      return;
    }
    _release();
    if (widget.native.send(
      viewer,
      DesktopAction.select,
      generation: _info.generation,
      display: display,
    )) {
      setState(() => _switching = true);
      _switchTimeout?.cancel();
      _switchTimeout = Timer(const Duration(seconds: 3), () {
        if (mounted && _switching) setState(() => _switching = false);
      });
    }
  }

  void _shortcut(List<int> keys) {
    final viewer = _viewer;
    if (viewer == null || !_controlling || _switching || _frames.status != 2) {
      return;
    }
    for (final key in keys) {
      widget.native.send(
        viewer,
        DesktopAction.key,
        generation: _info.generation,
        code: key,
        down: true,
      );
    }
    for (final key in keys.reversed) {
      widget.native.send(
        viewer,
        DesktopAction.key,
        generation: _info.generation,
        code: key,
      );
    }
  }

  Future<void> _fullscreen(bool enabled) async {
    _release();
    try {
      await widget.native.preview.setFullscreen(enabled);
      if (mounted) setState(() => _full = enabled);
    } on PlatformException {
      if (mounted) {
        ScaffoldMessenger.of(context)
            .showSnackBar(const SnackBar(content: Text('无法切换全屏。')));
      }
    } on MissingPluginException {
      if (mounted) {
        ScaffoldMessenger.of(context)
            .showSnackBar(const SnackBar(content: Text('当前构建缺少全屏支持。')));
      }
    }
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    SessionWindow.onCloseRequested(null);
    _poll?.cancel();
    _switchTimeout?.cancel();
    _release();
    if (_full) {
      unawaited(
        widget.native.preview.setFullscreen(false).catchError((Object _) {}),
      );
    }
    if (_viewer case final viewer?) {
      unawaited(widget.native.preview.stop(viewer));
    }
    _focus.dispose();
    super.dispose();
  }

  String get _targetLabel => widget.launch.label;
  bool get _controlling => _info.canInput && !_controlPaused;

  @override
  Widget build(BuildContext context) {
    final viewer = _viewer;
    final texture = _texture;
    final active = _frames.status == 2;
    return Scaffold(
      body: SafeArea(
        child: viewer == null
            ? _login()
            : FloatingSessionMenu(
                expanded: _menuOpen,
                onToggle: () => _setMenu(!_menuOpen),
                surface: active && texture != null
                    ? _surface(viewer, texture)
                    : _waiting(viewer),
                controls: _controls(viewer, active),
              ),
      ),
    );
  }

  void _setMenu(bool open) {
    if (!mounted || _viewer == null || open == _menuOpen) return;
    if (open) {
      _focus.unfocus();
      _release();
    }
    setState(() => _menuOpen = open);
  }

  void _toggleFullscreen() {
    _setMenu(false);
    unawaited(_fullscreen(!_full));
  }

  Widget _surface(int viewer, int texture) {
    final notice =
        _switching ||
        const {
          'display_changed',
          'capture_unavailable',
          'input_unavailable',
        }.contains(_info.notice);
    final colors = Theme.of(context).colorScheme;
    return Stack(
      fit: StackFit.expand,
      children: [
        ColoredBox(
          color: Colors.black,
          child: RemoteSurface(
            send: widget.native.send,
            viewer: viewer,
            texture: texture,
            focus: _focus,
            frameWidth: _frames.width,
            frameHeight: _frames.height,
            generation: _info.generation,
            canInput: _controlling && !_switching,
            scale: _scale,
          ),
        ),
        if (notice)
          Positioned(
            left: 12,
            right: 12,
            bottom: 12,
            child: Align(
              alignment: Alignment.centerLeft,
              child: IgnorePointer(
                child: Material(
                  color: colors.surface,
                  borderRadius: BorderRadius.circular(7),
                  child: Padding(
                    padding: const EdgeInsets.symmetric(
                      horizontal: 10,
                      vertical: 6,
                    ),
                    child: Semantics(
                      liveRegion: true,
                      child: Text(_currentStatus),
                    ),
                  ),
                ),
              ),
            ),
          ),
      ],
    );
  }

  Widget _waiting(int viewer) => Center(
    child: SingleChildScrollView(
      padding: const EdgeInsets.all(24),
      child: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          Text(_targetLabel, style: Theme.of(context).textTheme.titleLarge),
          const SizedBox(height: 12),
          Semantics(liveRegion: true, child: Text(_currentStatus)),
          const SizedBox(height: 12),
          if (_frames.status == 12) ...[
            const Text('请与被控端核对设备指纹。'),
            const SizedBox(height: 10),
            SelectableText(_info.identity ?? '', textAlign: TextAlign.center),
            const SizedBox(height: 12),
            FilledButton(
              onPressed: () => widget.native.trust(viewer),
              child: const Text('指纹一致，继续连接'),
            ),
          ] else
            const SizedBox.square(
              dimension: 20,
              child: CircularProgressIndicator(strokeWidth: 2),
            ),
        ],
      ),
    ),
  );

  Widget _controls(int viewer, bool active) {
    final theme = Theme.of(context);
    final resolution = active && _frames.width > 0
        ? ' · ${_frames.width} × ${_frames.height}'
        : '';
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Text(_targetLabel, style: theme.textTheme.titleMedium),
        const SizedBox(height: 2),
        Text('$_currentStatus$resolution', style: theme.textTheme.bodySmall),
        if (_info.storageFailed)
          Text('系统凭据或近期连接记录未保存。', style: theme.textTheme.bodySmall),
        if (active) ...[
          const Divider(height: 18),
          if (_info.displays.isNotEmpty)
            DropdownButtonFormField<String>(
              key: ValueKey(_info.selected),
              initialValue:
                  _info.displays.any((item) => item.id == _info.selected)
                  ? _info.selected
                  : null,
              isExpanded: true,
              decoration: const InputDecoration(labelText: '显示器'),
              onChanged: _switching ? null : _selectDisplay,
              items: [
                for (final display in _info.displays)
                  DropdownMenuItem(
                    value: display.id,
                    child: Text(display.label, overflow: TextOverflow.ellipsis),
                  ),
              ],
            ),
          const SizedBox(height: 4),
          Wrap(
            spacing: 6,
            runSpacing: 4,
            crossAxisAlignment: WrapCrossAlignment.center,
            children: [
              PopupMenuButton<SurfaceScale>(
                tooltip: '缩放',
                initialValue: _scale,
                onSelected: (scale) {
                  _release();
                  setState(() => _scale = scale);
                },
                itemBuilder: (_) => const [
                  PopupMenuItem(value: SurfaceScale.fit, child: Text('适配窗口')),
                  PopupMenuItem(
                    value: SurfaceScale.original,
                    child: Text('原始像素'),
                  ),
                  PopupMenuItem(
                    value: SurfaceScale.enlarge,
                    child: Text('放大到 200%'),
                  ),
                ],
                child: const Padding(
                  padding: EdgeInsets.symmetric(vertical: 8, horizontal: 7),
                  child: Text('缩放'),
                ),
              ),
              if (_controlling)
                PopupMenuButton<List<int>>(
                  tooltip: '发送组合键',
                  onSelected: _shortcut,
                  itemBuilder: (_) => const [
                    PopupMenuItem(
                      value: [0x000700e2, 0x0007002b],
                      child: Text('Alt + Tab'),
                    ),
                    PopupMenuItem(
                      value: [0x000700e0, 0x00070029],
                      child: Text('Ctrl + Esc'),
                    ),
                    PopupMenuItem(
                      value: [0x000700e3, 0x0007002b],
                      child: Text('Command / Win + Tab'),
                    ),
                  ],
                  child: const Padding(
                    padding: EdgeInsets.symmetric(vertical: 8, horizontal: 7),
                    child: Text('组合键'),
                  ),
                ),
              if (_info.canInput)
                TextButton.icon(
                  onPressed: () {
                    _release();
                    setState(() => _controlPaused = !_controlPaused);
                    _setMenu(false);
                  },
                  icon: const Icon(Icons.mouse_outlined),
                  label: Text(_controlPaused ? '恢复控制' : '暂停控制'),
                ),
              TextButton.icon(
                onPressed: _toggleFullscreen,
                icon: Icon(_full ? Icons.fullscreen_exit : Icons.fullscreen),
                label: Text(_full ? '退出全屏' : '全屏'),
              ),
            ],
          ),
          if (widget.native.collaboration case final collaboration?)
            SessionTools(
              native: collaboration,
              viewer: viewer,
              display: _info.selected,
              generation: _info.generation,
              frameWidth: _frames.width,
              frameHeight: _frames.height,
              canInput: _controlling && !_switching,
              hasControlGrant: _info.canInput,
              release: _release,
              onModePreview: () => _setMenu(true),
              defaultQuality: widget.launch.settings.videoQuality,
              scale: _scale,
              downloadRoot: widget.launch.settings.downloadRoot,
            ),
        ],
        if (!active && _full)
          Align(
            alignment: Alignment.centerLeft,
            child: TextButton.icon(
              onPressed: _toggleFullscreen,
              icon: const Icon(Icons.fullscreen_exit),
              label: const Text('退出全屏'),
            ),
          ),
        const Divider(height: 18),
        Align(
          alignment: Alignment.centerRight,
          child: OutlinedButton.icon(
            onPressed: _confirmClose,
            icon: const Icon(Icons.link_off_rounded, size: 18),
            label: const Text('断开连接'),
            style: OutlinedButton.styleFrom(
              foregroundColor: theme.colorScheme.error,
            ),
          ),
        ),
      ],
    );
  }

  String get _currentStatus => switch (_frames.status) {
    2 when _switching => '正在切换显示器…',
    2 when _info.notice == 'display_changed' => '显示器发生变化，已切换到可用来源',
    2 when _info.notice == 'capture_unavailable' => '无法切换到该显示器',
    2 when _info.notice == 'input_unavailable' => '系统拒绝了键鼠权限，当前仅观看',
    2 when _info.canInput && _controlPaused => '已连接 · 控制已暂停',
    2 => _info.canInput ? '已连接 · 可控制' : '已连接 · 仅观看',
    _ => sessionProgress(_frames.status),
  };

  Widget _login() => Center(
    child: Padding(
      padding: const EdgeInsets.all(24),
      child: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          Text(_targetLabel, style: Theme.of(context).textTheme.titleLarge),
          const SizedBox(height: 12),
          if (_issue case final issue?) ...[
            Semantics(
              liveRegion: true,
              child: Text(issue, textAlign: TextAlign.center),
            ),
            const SizedBox(height: 16),
          ],
          OverflowBar(
            alignment: MainAxisAlignment.center,
            spacing: 8,
            overflowSpacing: 6,
            children: [
              TextButton(onPressed: _confirmClose, child: const Text('关闭窗口')),
              FilledButton(onPressed: _reconnect, child: const Text('重新连接')),
            ],
          ),
        ],
      ),
    ),
  );
}
