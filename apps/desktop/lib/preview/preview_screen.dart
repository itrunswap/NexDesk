import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import 'native_preview.dart';

class PreviewScreen extends StatefulWidget {
  const PreviewScreen({
    super.key,
    required this.backend,
    required this.address,
    required this.port,
  });

  final NativePreview backend;
  final String address;
  final int port;

  @override
  State<PreviewScreen> createState() => _PreviewScreenState();
}

class _PreviewScreenState extends State<PreviewScreen> {
  final _code = TextEditingController();
  final _identity = TextEditingController();
  int? _viewer;
  int? _texture;
  Timer? _poll;
  String? _issue;
  PreviewState _state = const PreviewState();

  Future<void> _start() async {
    if (_viewer != null) return;
    final id = widget.backend.connect(
      widget.address,
      widget.port,
      _code.text,
      _identity.text.trim(),
    );
    _code.clear();
    if (id == 0) {
      setState(() => _issue = '请检查六位连接密码和可选的设备指纹。');
      return;
    }
    setState(() {
      _viewer = id;
      _issue = null;
      _state = const PreviewState(status: 1);
    });
    try {
      final texture = await widget.backend.attachTexture(id);
      if (!mounted || _viewer != id) {
        await widget.backend.stop(id);
        return;
      }
      if (texture == null) {
        _stop('无法创建原生视频纹理。');
        return;
      }
      setState(() => _texture = texture);
      _poll = Timer.periodic(const Duration(milliseconds: 250), _refresh);
    } on PlatformException {
      if (mounted && _viewer == id) _stop('无法创建原生视频纹理。');
    } on MissingPluginException {
      if (mounted && _viewer == id) _stop('请使用包含 M1 纹理桥接的桌面构建。');
    }
  }

  void _refresh(Timer timer) {
    final id = _viewer;
    if (id == null) return;
    final state = widget.backend.inspect(id);
    final error = switch (state.status) {
      1 || 2 => null,
      3 => '预览已结束。',
      4 => '主机未通过配对。请确认六位密码仍然有效。',
      5 => '被控设备的身份发生了变化。请核对主机显示的指纹。',
      6 => '无法连接被控电脑。请检查地址、端口和主机进程。',
      8 => '主机正在限制新连接，请稍后重试。',
      _ => '媒体协商失败，或在时限内没有收到首帧。',
    };
    if (error != null) {
      _stop(error);
    } else {
      setState(() => _state = state);
    }
  }

  void _stop([String? issue]) {
    final id = _viewer;
    _poll?.cancel();
    _poll = null;
    if (id != null) unawaited(widget.backend.stop(id));
    setState(() {
      _viewer = null;
      _texture = null;
      _state = const PreviewState();
      _issue = issue;
    });
  }

  @override
  void dispose() {
    _poll?.cancel();
    final id = _viewer;
    if (id != null) unawaited(widget.backend.stop(id));
    _code.dispose();
    _identity.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final address =
        widget.address.contains(':') && !widget.address.startsWith('[')
        ? '[${widget.address}]'
        : widget.address;
    return Scaffold(
      appBar: AppBar(
        title: const Text('只读预览'),
        actions: [
          if (_viewer != null)
            TextButton(onPressed: _stop, child: const Text('结束观看')),
          const SizedBox(width: 12),
        ],
      ),
      body: Align(
        alignment: Alignment.topCenter,
        child: SingleChildScrollView(
          padding: const EdgeInsets.all(24),
          child: ConstrainedBox(
            constraints: BoxConstraints(maxWidth: _viewer == null ? 460 : 1040),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                Text('$address:${widget.port}'),
                const SizedBox(height: 16),
                if (_viewer == null) ...[
                  TextField(
                    controller: _code,
                    obscureText: true,
                    enableSuggestions: false,
                    autocorrect: false,
                    autofillHints: const [],
                    inputFormatters: [LengthLimitingTextInputFormatter(6)],
                    decoration: const InputDecoration(labelText: '六位连接密码'),
                    onSubmitted: (_) => _start(),
                  ),
                  const SizedBox(height: 12),
                  TextField(
                    controller: _identity,
                    decoration: const InputDecoration(labelText: '设备指纹（可选）'),
                    onSubmitted: (_) => _start(),
                  ),
                  const SizedBox(height: 16),
                  FilledButton(onPressed: _start, child: const Text('开始观看')),
                ] else ...[
                  Text(_state.status == 2 ? '局域网直连 · 只读' : '正在配对并等待首帧…'),
                  const SizedBox(height: 12),
                  if (_texture case final texture?)
                    AspectRatio(
                      aspectRatio: _state.height > 0
                          ? _state.width / _state.height
                          : 16 / 9,
                      child: ColoredBox(
                        color: Colors.black,
                        child: Texture(textureId: texture),
                      ),
                    ),
                  if (_state.presented > 0) ...[
                    const SizedBox(height: 12),
                    Text(
                      '${_state.width} × ${_state.height} · 纹理取帧 ${_state.presented} · '
                      '解码到纹理 ${(_state.textureWaitUs / 1000).toStringAsFixed(1)} ms',
                    ),
                  ],
                ],
                if (_issue case final issue?) ...[
                  const SizedBox(height: 12),
                  Semantics(liveRegion: true, child: Text(issue)),
                ],
              ],
            ),
          ),
        ),
      ),
    );
  }
}
