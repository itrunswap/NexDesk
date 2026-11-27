import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';

import '../connection/video_quality.dart';
import '../connection/video_quality_fields.dart';
import 'native_collaboration.dart';
import 'remote_surface.dart';

typedef AppliedSessionQuality = ({
  VideoQuality quality,
  SurfaceScale scale,
  bool liftShadows,
  bool sent,
  bool complete,
});

class SessionQualityDialog extends StatefulWidget {
  const SessionQualityDialog({
    super.key,
    required this.initial,
    required this.remoteConfirmed,
    required this.scale,
    this.liftShadows = false,
    required this.readDecoded,
    required this.readSource,
    required this.apply,
  });

  final VideoQuality initial;
  final bool remoteConfirmed;
  final SurfaceScale scale;
  final bool liftShadows;
  final Size Function() readDecoded;
  final Future<Size> Function() readSource;
  final Future<VideoQuality> Function(VideoQuality) apply;

  @override
  State<SessionQualityDialog> createState() => _SessionQualityDialogState();
}

class _SessionQualityDialogState extends State<SessionQualityDialog> {
  late VideoQuality _draft = widget.initial;
  late SurfaceScale _scale = widget.scale;
  late bool _liftShadows = widget.liftShadows;
  late Size _decoded = widget.readDecoded();
  Size _source = Size.zero;
  Timer? _timer;
  bool _busy = false;
  String? _error;

  @override
  void initState() {
    super.initState();
    unawaited(_loadSource());
    _timer = Timer.periodic(const Duration(seconds: 1), (_) {
      final size = widget.readDecoded();
      if (size != _decoded) setState(() => _decoded = size);
    });
  }

  Future<void> _loadSource() async {
    try {
      final size = await widget.readSource();
      if (mounted) setState(() => _source = size);
    } on CollaborationIssue {
      // Older hosts may provide video without sender diagnostics.
    }
  }

  @override
  void dispose() {
    _timer?.cancel();
    super.dispose();
  }

  Future<void> _apply() async {
    if (_busy) return;
    setState(() {
      _busy = true;
      _error = null;
    });
    final requested = _draft;
    final scale = _scale;
    final localOnly =
        requested.matches(widget.initial) &&
        (widget.remoteConfirmed ||
            scale != widget.scale ||
            _liftShadows != widget.liftShadows);
    try {
      final applied = localOnly
          ? widget.initial
          : await widget.apply(requested);
      if (!mounted) return;
      Navigator.pop<AppliedSessionQuality>(context, (
        quality: applied,
        scale: scale,
        liftShadows: _liftShadows,
        sent: !localOnly,
        complete: applied.matches(requested),
      ));
    } on CollaborationIssue catch (error) {
      if (mounted) {
        setState(() {
          _busy = false;
          _error = '被控端未确认。${error.message}';
        });
      }
    }
  }

  @override
  Widget build(BuildContext context) {
    final small = Theme.of(context).textTheme.bodySmall;
    final limits = _draft.resolution;
    final ratio = MediaQuery.devicePixelRatioOf(context);
    final display = _decoded.isEmpty
        ? null
        : remoteFrameLayout(sessionViewport(context), _decoded, _scale, ratio);
    String dimensions(Size size) =>
        '${size.width.round()} × ${size.height.round()}';
    return PopScope(
      canPop: !_busy,
      child: AlertDialog(
        title: const Text('画质与帧率'),
        scrollable: true,
        content: SizedBox(
          width: 380,
          child: AbsorbPointer(
            absorbing: _busy,
            child: Column(
              mainAxisSize: MainAxisSize.min,
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                Text(
                  [
                    if (!_source.isEmpty) '远端采集 ${dimensions(_source)}',
                    if (!_decoded.isEmpty)
                      '当前接收 ${dimensions(_decoded)}'
                    else
                      '等待解码画面',
                  ].join('  ·  '),
                  style: small,
                ),
                Align(
                  alignment: Alignment.centerLeft,
                  child: TextButton.icon(
                    icon: const Icon(Icons.text_fields, size: 18),
                    label: const Text('使用原屏文字'),
                    onPressed: _busy
                        ? null
                        : () => setState(() {
                            _draft = _draft
                                .withProfile(VideoProfile.clear)
                                .withCaps(resolution: StreamResolution.native);
                          }),
                  ),
                ),
                VideoQualityFields(
                  value: _draft,
                  onChanged: (v) => setState(() => _draft = v),
                ),
                if (!_source.isEmpty && limits != StreamResolution.native)
                  Padding(
                    padding: const EdgeInsets.only(top: 4),
                    child: Text(
                      limits.limits(
                            _source.width.toInt(),
                            _source.height.toInt(),
                          )
                          ? '这个上限会缩小远端画面。选择「按远端尺寸」可保留原屏分辨率。'
                          : '这个上限已覆盖远端画面，提高上限会保留相同的像素数。',
                      style: small,
                    ),
                  ),
                const SizedBox(height: 12),
                InputDecorator(
                  decoration: const InputDecoration(labelText: '本地显示'),
                  child: DropdownButtonHideUnderline(
                    child: DropdownButton<SurfaceScale>(
                      value: _scale,
                      isDense: true,
                      isExpanded: true,
                      items: [
                        for (final mode in SurfaceScale.values)
                          DropdownMenuItem(
                            value: mode,
                            child: Text(mode.label),
                          ),
                      ],
                      onChanged: (mode) {
                        if (mode != null) setState(() => _scale = mode);
                      },
                    ),
                  ),
                ),
                if (display != null)
                  Padding(
                    padding: const EdgeInsets.only(top: 4),
                    child: Text(
                      '${dimensions(display.size * ratio)} 屏幕像素 · ${(display.physicalScale * 100).toStringAsFixed(1)}%'
                      '${ratio > 1 ? '\n系统倍率 ${ratio.toStringAsFixed(2)}×，占用 ${dimensions(display.size)} 逻辑点' : ''}',
                      style: small,
                    ),
                  ),
                if (_scale == SurfaceScale.original)
                  Padding(
                    padding: const EdgeInsets.only(top: 4),
                    child: Text('每个解码像素占一个屏幕像素。在高分屏上阅读可选「适配窗口」。', style: small),
                  ),
                if (defaultTargetPlatform == TargetPlatform.macOS)
                  SwitchListTile(
                    contentPadding: EdgeInsets.zero,
                    title: const Text('提亮暗部'),
                    subtitle: const Text('仅调整此 Mac 会话的阴影，默认显示解码原色。'),
                    value: _liftShadows,
                    onChanged: (value) => setState(() => _liftShadows = value),
                  ),
                if (_error != null)
                  Padding(
                    padding: const EdgeInsets.only(top: 8),
                    child: Semantics(
                      liveRegion: true,
                      child: Text(
                        _error!,
                        style: TextStyle(
                          color: Theme.of(context).colorScheme.error,
                        ),
                      ),
                    ),
                  ),
              ],
            ),
          ),
        ),
        actions: [
          TextButton(
            onPressed: _busy ? null : () => Navigator.pop(context),
            child: const Text('取消'),
          ),
          FilledButton(
            onPressed: _busy ? null : _apply,
            child: Text(_busy ? '等待被控端确认' : '应用'),
          ),
        ],
      ),
    );
  }
}
