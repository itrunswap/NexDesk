import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import 'file_panel.dart';
import 'native_collaboration.dart';

class SessionTools extends StatefulWidget {
  const SessionTools({
    super.key,
    required this.native,
    required this.viewer,
    required this.display,
    required this.generation,
    required this.frameWidth,
    required this.frameHeight,
    required this.canInput,
    required this.release,
    required this.onModePreview,
    this.downloadRoot = '',
  });

  final NativeCollaboration native;
  final int viewer;
  final String display;
  final int generation, frameWidth, frameHeight;
  final bool canInput;
  final VoidCallback release, onModePreview;
  final String downloadRoot;

  @override
  State<SessionTools> createState() => _SessionToolsState();
}

class _SessionToolsState extends State<SessionTools> {
  Map<String, Object?> _state = const {};
  Map<String, Object?>? _preview;
  Timer? _timer;
  DateTime? _expires;
  String? _checked;
  bool _modeBusy = false;
  bool _remember = false;
  String _preset = 'auto';
  int _fps = 30;
  int _previousVolume = 60;

  int? get _grant => _map(_state['session'])['capabilities'] as int?;
  int get _volume => _state['volume'] as int? ?? 0;
  bool _can(CollaborationCapability value) => (_grant ?? 0) & value.bit != 0;

  @override
  void initState() {
    super.initState();
    _state = widget.native.inspect(widget.viewer);
    _timer = Timer.periodic(const Duration(milliseconds: 400), (_) {
      _refresh();
    });
  }

  @override
  void didUpdateWidget(SessionTools oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (oldWidget.display != widget.display) {
      _checked = null;
      _preview = null;
    }
  }

  @override
  void dispose() {
    _timer?.cancel();
    super.dispose();
  }

  void _refresh() {
    setState(() {
      _state = widget.native.inspect(widget.viewer);
      final quality = _map(_map(_state['session'])['quality']);
      _preset = quality['preset'] as String? ?? _preset;
      _fps = quality['fps'] as int? ?? _fps;
      if (_expires case final expiry? when DateTime.now().isAfter(expiry)) {
        _preview = null;
        _expires = null;
      }
    });
    if (_can(CollaborationCapability.display) &&
        widget.display.isNotEmpty &&
        _checked != widget.display &&
        !_modeBusy &&
        _preview == null) {
      _checked = widget.display;
      unawaited(_remembered(widget.display));
    }
  }

  Future<void> _remembered(String display) async {
    try {
      final response = await widget.native.remote(
        widget.viewer,
        'modes',
        fields: {'display': display},
      );
      if (!mounted ||
          widget.display != display ||
          _modeBusy ||
          _preview != null) {
        return;
      }
      final remembered = _map(response['remembered']);
      final current = _map(_map(response['ok'])['current']);
      if (remembered.isNotEmpty && !_same(remembered, current)) {
        await _apply(display, remembered, remember: true);
      }
    } on CollaborationIssue {
      // A display may have disappeared while its saved mode was being checked.
    }
  }

  Future<void> _run(Future<void> Function() action) async {
    try {
      await action();
      if (mounted) _refresh();
    } on CollaborationIssue catch (error) {
      if (mounted) _message(error.message);
    } on PlatformException {
      if (mounted) _message('系统无法访问剪贴板。');
    }
  }

  void _message(String text) {
    ScaffoldMessenger.of(context)
      ..hideCurrentSnackBar()
      ..showSnackBar(SnackBar(content: Text(text)));
  }

  Future<void> _quality() => _run(() async {
    widget.release();
    var preset = _preset;
    var fps = _fps;
    final selected = await showDialog<bool>(
      context: context,
      builder: (context) => StatefulBuilder(
        builder: (context, update) => AlertDialog(
          title: const Text('画质与帧率'),
          content: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              DropdownButtonFormField<String>(
                initialValue: preset,
                decoration: const InputDecoration(labelText: '画质模式'),
                items: const [
                  DropdownMenuItem(value: 'auto', child: Text('自动')),
                  DropdownMenuItem(value: 'clear', child: Text('清晰')),
                  DropdownMenuItem(value: 'smooth', child: Text('流畅')),
                ],
                onChanged: (value) => update(() => preset = value ?? 'auto'),
              ),
              const SizedBox(height: 12),
              DropdownButtonFormField<int>(
                initialValue: fps,
                decoration: const InputDecoration(labelText: '帧率上限'),
                items: [15, 24, 30, 45, 60]
                    .map(
                      (value) => DropdownMenuItem(
                        value: value,
                        child: Text('$value FPS'),
                      ),
                    )
                    .toList(),
                onChanged: (value) => update(() => fps = value ?? 30),
              ),
            ],
          ),
          actions: [
            TextButton(
              onPressed: () => Navigator.pop(context),
              child: const Text('取消'),
            ),
            FilledButton(
              onPressed: () => Navigator.pop(context, true),
              child: const Text('应用'),
            ),
          ],
        ),
      ),
    );
    if (!mounted || selected != true) return;
    await widget.native.remote(
      widget.viewer,
      'quality',
      fields: {
        'quality': {'preset': preset, 'fps': fps},
      },
    );
    if (!mounted) return;
    setState(() {
      _preset = preset;
      _fps = fps;
    });
  });

  Future<void> _chooseMode() => _run(() async {
    if (_modeBusy || _preview != null) return;
    final display = widget.display;
    widget.release();
    final response = await widget.native.remote(
      widget.viewer,
      'modes',
      fields: {'display': display},
    );
    if (!mounted || widget.display != display) return;
    final data = _map(response['ok']);
    final current = _map(data['current']);
    final modes = (data['choices'] as List? ?? []).map(_map).toList();
    final selected = await showDialog<Map<String, Object?>>(
      context: context,
      builder: (context) => SimpleDialog(
        title: Text('分辨率 · ${_modeName(current)}'),
        children: [
          for (final mode in modes)
            SimpleDialogOption(
              onPressed: () => Navigator.pop(context, mode),
              child: Text(_modeName(mode)),
            ),
        ],
      ),
    );
    if (!mounted || selected == null || widget.display != display) return;
    await _apply(
      display,
      selected,
      remember: _same(selected, _map(response['remembered'])),
    );
  });

  Future<void> _apply(
    String display,
    Map<String, Object?> mode, {
    bool remember = false,
  }) async {
    if (_modeBusy || _preview != null) return;
    widget.release();
    setState(() => _modeBusy = true);
    try {
      final response = await widget.native.remote(
        widget.viewer,
        'apply',
        fields: {'display': display, 'mode': mode},
      );
      if (!mounted) return;
      final result = _map(response['ok']);
      setState(() {
        _preview = result;
        _remember = remember;
        _expires = DateTime.now().add(
          Duration(milliseconds: result['expires_ms'] as int? ?? 14000),
        );
      });
      widget.onModePreview();
    } finally {
      if (mounted) setState(() => _modeBusy = false);
    }
  }

  Future<void> _finishMode(bool confirm) => _run(() async {
    final token = _preview?['token'] as int?;
    if (token == null || _modeBusy) return;
    setState(() => _modeBusy = true);
    try {
      final response = await widget.native.remote(
        widget.viewer,
        confirm ? 'confirm' : 'rollback',
        fields: {'token': token},
        remember: confirm && _remember,
      );
      if (mounted) {
        setState(() {
          _preview = null;
          _expires = null;
        });
        if (response['warning'] != null) _message('画面已确认，本机无法记住此分辨率。');
      }
    } finally {
      if (mounted) setState(() => _modeBusy = false);
    }
  });

  Future<void> _clipboard(String task) => _run(() async {
    widget.release();
    if (task == 'send') {
      final text = (await Clipboard.getData(Clipboard.kTextPlain))?.text ?? '';
      await widget.native.remote(
        widget.viewer,
        'clipboard_write',
        fields: {'text': text},
      );
      if (mounted) _message('文本已发送到被控端。');
    } else {
      final reply = await widget.native.remote(widget.viewer, 'clipboard_read');
      final text = _map(reply['ok'])['text'] as String? ?? '';
      await Clipboard.setData(ClipboardData(text: text));
      if (mounted) _message('已复制被控端文本。');
    }
  });

  Future<void> _text() => _run(() async {
    widget.release();
    final generation = widget.generation;
    final text = await showDialog<String>(
      context: context,
      builder: (_) => const _CommittedText(),
    );
    if (mounted &&
        text != null &&
        generation == widget.generation &&
        widget.canInput) {
      await widget.native.remote(
        widget.viewer,
        'text',
        fields: {'generation': generation, 'text': text},
      );
    }
  });

  Future<void> _setVolume(int level) => _run(() async {
    if (level > 0) _previousVolume = level;
    await widget.native.request(widget.viewer, {
      'operation': 'volume',
      'level': level,
    });
  });

  Future<void> _volumePanel() => _run(() async {
    widget.release();
    var level = _volume.toDouble();
    final result = await showDialog<int>(
      context: context,
      builder: (context) => StatefulBuilder(
        builder: (context, update) => AlertDialog(
          title: const Text('远端音量'),
          content: Slider(
            value: level,
            max: 100,
            divisions: 20,
            label: '${level.round()}%',
            onChanged: (value) => update(() => level = value),
          ),
          actions: [
            TextButton(
              onPressed: () => Navigator.pop(context, 0),
              child: const Text('静音'),
            ),
            FilledButton(
              onPressed: () => Navigator.pop(context, level.round()),
              child: const Text('应用'),
            ),
          ],
        ),
      ),
    );
    if (mounted && result != null) await _setVolume(result);
  });

  void _files() {
    widget.release();
    showDialog<void>(
      context: context,
      builder: (_) => FilePanel(
        initialDirectory: widget.downloadRoot,
        native: widget.native,
        viewer: widget.viewer,
        capabilities: _grant ?? 0,
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    if (_grant == null) return const SizedBox.shrink();
    final hasFiles =
        _can(CollaborationCapability.fileRead) ||
        _can(CollaborationCapability.fileWrite);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Wrap(
          spacing: 6,
          runSpacing: 4,
          crossAxisAlignment: WrapCrossAlignment.center,
          children: [
            TextButton.icon(
              icon: const Icon(Icons.tune),
              label: const Text('画质'),
              onPressed: _quality,
            ),
            if (_can(CollaborationCapability.display) &&
                widget.display.isNotEmpty)
              TextButton(
                onPressed: _modeBusy || _preview != null ? null : _chooseMode,
                child: const Text('分辨率'),
              ),
            if (_can(CollaborationCapability.audio)) ...[
              IconButton(
                tooltip: _volume == 0 ? '恢复远端声音' : '静音',
                onPressed: () => _setVolume(_volume == 0 ? _previousVolume : 0),
                icon: Icon(_volume == 0 ? Icons.volume_off : Icons.volume_up),
              ),
              TextButton(onPressed: _volumePanel, child: const Text('音量')),
            ],
            if (_can(CollaborationCapability.clipboard))
              PopupMenuButton<String>(
                tooltip: '文本剪贴板',
                onSelected: _clipboard,
                icon: const Icon(Icons.content_paste),
                itemBuilder: (context) => const [
                  PopupMenuItem(value: 'send', child: Text('发送本机剪贴板文本')),
                  PopupMenuItem(value: 'read', child: Text('取回远端剪贴板文本')),
                ],
              ),
            if (_can(CollaborationCapability.text) && widget.canInput)
              TextButton(onPressed: _text, child: const Text('输入文本')),
            if (!widget.canInput)
              TextButton(
                onPressed: () => _run(() async {
                  await widget.native.request(widget.viewer, {
                    'operation': 'request_control',
                  });
                }),
                child: const Text('申请控制'),
              ),
            if (hasFiles)
              TextButton.icon(
                onPressed: _files,
                icon: const Icon(Icons.folder_outlined),
                label: const Text('文件'),
              ),
            IconButton(
              tooltip: '连接与媒体统计',
              icon: const Icon(Icons.monitor_heart_outlined),
              onPressed: () {
                widget.release();
                showDialog<void>(
                  context: context,
                  builder: (_) => _Diagnostics(
                    native: widget.native,
                    viewer: widget.viewer,
                  ),
                );
              },
            ),
          ],
        ),
        if (_preview case final preview?) _previewBar(preview),
      ],
    );
  }

  Widget _previewBar(Map<String, Object?> preview) {
    final mode = _map(preview['mode']);
    final end = _expires ?? DateTime.now();
    final seconds =
        ((end.difference(DateTime.now()).inMilliseconds + 999) ~/ 1000).clamp(
          0,
          15,
        );
    final newFrame =
        widget.frameWidth > 0 &&
        widget.frameHeight > 0 &&
        (widget.frameWidth * (mode['height'] as int) -
                    widget.frameHeight * (mode['width'] as int))
                .abs() <=
            widget.frameWidth * 4;
    return Wrap(
      spacing: 8,
      crossAxisAlignment: WrapCrossAlignment.center,
      children: [
        Text('保留 ${_modeName(mode)}？$seconds 秒后恢复'),
        FilterChip(
          label: const Text('记住此选择'),
          selected: _remember,
          onSelected: (value) => setState(() => _remember = value),
        ),
        FilledButton(
          onPressed: !_modeBusy && newFrame ? () => _finishMode(true) : null,
          child: const Text('保留'),
        ),
        TextButton(
          onPressed: _modeBusy ? null : () => _finishMode(false),
          child: const Text('恢复'),
        ),
      ],
    );
  }
}

class _CommittedText extends StatefulWidget {
  const _CommittedText();
  @override
  State<_CommittedText> createState() => _CommittedTextState();
}

class _CommittedTextState extends State<_CommittedText> {
  final _text = TextEditingController();
  @override
  void dispose() {
    _text.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => AlertDialog(
    title: const Text('输入文本'),
    content: TextField(
      autofocus: true,
      controller: _text,
      maxLength: 1200,
      minLines: 3,
      maxLines: 8,
      decoration: const InputDecoration(labelText: '发送给被控端当前输入位置'),
    ),
    actions: [
      TextButton(
        onPressed: () => Navigator.pop(context),
        child: const Text('取消'),
      ),
      FilledButton(
        onPressed: () => Navigator.pop(context, _text.text),
        child: const Text('发送'),
      ),
    ],
  );
}

class _Diagnostics extends StatefulWidget {
  const _Diagnostics({required this.native, required this.viewer});
  final NativeCollaboration native;
  final int viewer;

  @override
  State<_Diagnostics> createState() => _DiagnosticsState();
}

class _DiagnosticsState extends State<_Diagnostics> {
  Timer? _clock;
  bool _loading = false;
  Map<String, Object?> _sent = const {}, _info = const {};

  @override
  void initState() {
    super.initState();
    unawaited(_refresh());
    _clock = Timer.periodic(const Duration(seconds: 2), (_) => _refresh());
  }

  Future<void> _refresh() async {
    if (_loading) return;
    _loading = true;
    try {
      final reply = await widget.native.remote(widget.viewer, 'diagnostics');
      if (mounted) {
        setState(() {
          _sent = _map(reply['ok']);
          _info = _map(widget.native.inspect(widget.viewer)['session']);
        });
      }
    } on CollaborationIssue {
      if (mounted) {
        setState(() {
          _sent = const {};
          _info = const {};
        });
      }
    } finally {
      _loading = false;
    }
  }

  @override
  void dispose() {
    _clock?.cancel();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final received = _map(_info['media']);
    final sender = _map(_sent['media']);
    final ice = _map(received['ice']);
    final audio = _map(_sent['audio']);
    final sound = _map(_info['sound']);
    return AlertDialog(
      title: const Text('连接与媒体统计'),
      content: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        mainAxisSize: MainAxisSize.min,
        children: [
          Text(
            '网络  ${ice.isEmpty
                ? '等待统计'
                : ice['relayed'] == true
                ? '中继'
                : '局域网直连'}',
          ),
          Text('网络 RTT  ${_number(ice['rtt_ms'])} ms'),
          Text(
            '接收  ${_number(_info['frames_per_second'])} FPS · ${_number(_info['kilobits_per_second'])} kbit/s',
          ),
          Text('编码  ${sender['codec_implementation'] ?? '—'}'),
          Text('解码  ${received['codec_implementation'] ?? '—'}'),
          Text(
            '丢帧  ${received['dropped_frames'] ?? '—'} · 丢包  ${received['lost_packets'] ?? '—'}',
          ),
          Text('平均抖动缓冲  ${_number(received['average_jitter_ms'])} ms'),
          Text(
            '系统声音  ${audio['available'] == true ? '采集中' : '未采集'} · 解码 ${sound['decoded'] ?? 0} 帧',
          ),
        ],
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.pop(context),
          child: const Text('关闭'),
        ),
      ],
    );
  }
}

Map<String, Object?> _map(Object? value) =>
    value is Map ? value.cast<String, Object?>() : const {};
bool _same(Map first, Map second) => [
  'width',
  'height',
  'refresh_millihz',
].every((key) => first[key] == second[key]);
String _number(Object? item) => item is num ? item.toStringAsFixed(1) : '—';
String _modeName(Map mode) =>
    '${mode['width']} × ${mode['height']} · ${_number((mode['refresh_millihz'] as int? ?? 0) / 1000)} Hz';
