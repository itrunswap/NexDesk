import 'dart:async';
import 'dart:math' as math;

import 'package:flutter/foundation.dart';
import 'package:flutter/gestures.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import 'native_desktop.dart';

enum SurfaceScale {
  fit('适配窗口'),
  original('原始像素（物理 1:1）'),
  enlarge('放大到 200%');

  const SurfaceScale(this.label);
  final String label;
}

Size sessionViewport(BuildContext context) {
  final window = MediaQuery.sizeOf(context);
  final ratio = MediaQuery.devicePixelRatioOf(context);
  final padding = View.of(context).padding;
  return Size(
    window.width - (padding.left + padding.right) / ratio,
    window.height - (padding.top + padding.bottom) / ratio,
  );
}

({Size size, double physicalScale, FilterQuality filter}) remoteFrameLayout(
  Size viewport,
  Size frame,
  SurfaceScale scale,
  double pixelRatio,
) {
  final fit = math.min(
    viewport.width * pixelRatio / frame.width,
    viewport.height * pixelRatio / frame.height,
  );
  final whole = fit.floor();
  // At most 1% margin per side avoids filtering small type for a few spare pixels.
  final factor = switch (scale) {
    SurfaceScale.fit when whole >= 1 && fit <= whole * 1.02 => whole.toDouble(),
    SurfaceScale.fit => fit,
    SurfaceScale.original => 1.0,
    SurfaceScale.enlarge => 2.0,
  };
  final filter = factor >= 1 && factor == factor.roundToDouble()
      ? FilterQuality.none
      : factor > 0.5
      ? FilterQuality.high
      : FilterQuality.medium;
  return (
    size: frame * (factor / pixelRatio),
    physicalScale: factor,
    filter: filter,
  );
}

typedef DesktopActionSender = bool Function(
  int viewer,
  DesktopAction action, {
  int generation,
  int x,
  int y,
  int code,
  bool down,
});

class RemoteSurface extends StatefulWidget {
  const RemoteSurface({
    super.key,
    required this.send,
    required this.viewer,
    required this.texture,
    required this.focus,
    required this.frameWidth,
    required this.frameHeight,
    required this.generation,
    required this.canInput,
    this.scale = SurfaceScale.fit,
    this.liftShadows = false,
  });

  final DesktopActionSender send;
  final int viewer, texture, frameWidth, frameHeight, generation;
  final FocusNode focus;
  final bool canInput;
  final SurfaceScale scale;
  final bool liftShadows;

  @override
  State<RemoteSurface> createState() => _RemoteSurfaceState();
}

class _RemoteSurfaceState extends State<RemoteSurface>
    with WidgetsBindingObserver {
  final _motion = Stopwatch()..start();
  int _buttons = 0;
  int _lastMove = -100;
  Offset _wheelRemainder = Offset.zero;
  final _vertical = ScrollController();
  final _horizontal = ScrollController();

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
  }

  @override
  void didUpdateWidget(RemoteSurface old) {
    super.didUpdateWidget(old);
    if (old.canInput && !widget.canInput ||
        old.generation != widget.generation ||
        old.scale != widget.scale) {
      _release();
    }
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    if (state != AppLifecycleState.resumed) _release();
  }

  @override
  void dispose() {
    _release();
    WidgetsBinding.instance.removeObserver(this);
    _vertical.dispose();
    _horizontal.dispose();
    super.dispose();
  }

  void _release() {
    _buttons = 0;
    _wheelRemainder = Offset.zero;
    widget.send(widget.viewer, DesktopAction.release);
  }

  KeyEventResult _key(FocusNode _, KeyEvent event) {
    if (!widget.canInput) return KeyEventResult.ignored;
    final usage = event.physicalKey.usbHidUsage;
    if (usage < 0x00070004 || usage > 0x000700e7) return KeyEventResult.ignored;
    widget.send(
      widget.viewer,
      DesktopAction.key,
      generation: widget.generation,
      code: usage,
      down: event is! KeyUpEvent,
    );
    return KeyEventResult.handled;
  }

  (int, int) _point(Offset offset, Size size) {
    int axis(double value, double span) =>
        ((value / span).clamp(0.0, 1.0) * 65535).round();
    return (axis(offset.dx, size.width), axis(offset.dy, size.height));
  }

  void _pointer(PointerEvent event, Size size, {bool buttonEvent = false}) {
    if (!widget.canInput ||
        size.isEmpty ||
        event.kind != PointerDeviceKind.mouse) {
      return;
    }
    final (x, y) = _point(event.localPosition, size);
    final changed = _buttons ^ event.buttons;
    if (buttonEvent || changed != 0) {
      for (var index = 0; index < 5; index++) {
        final bit = 1 << index;
        if (changed & bit != 0) {
          widget.send(
            widget.viewer,
            DesktopAction.button,
            generation: widget.generation,
            x: x,
            y: y,
            code: index + 1,
            down: event.buttons & bit != 0,
          );
        }
      }
      _buttons = event.buttons;
      return;
    }
    final now = _motion.elapsedMilliseconds;
    if (now - _lastMove < 12) return;
    _lastMove = now;
    widget.send(
      widget.viewer,
      DesktopAction.move,
      generation: widget.generation,
      x: x,
      y: y,
    );
  }

  void _wheel(Offset position, Offset delta, Size size) {
    if (!widget.canInput || size.isEmpty || !delta.isFinite) return;
    final units = Offset(delta.dx, -delta.dy) * 3 + _wheelRemainder;
    final horizontal = units.dx.truncate();
    final vertical = units.dy.truncate();
    _wheelRemainder =
        units - Offset(horizontal.toDouble(), vertical.toDouble());
    if (horizontal == 0 && vertical == 0) return;

    final (x, y) = _point(position, size);
    widget.send(
      widget.viewer,
      DesktopAction.move,
      generation: widget.generation,
      x: x,
      y: y,
    );
    widget.send(
      widget.viewer,
      DesktopAction.wheel,
      generation: widget.generation,
      x: horizontal.clamp(-1200, 1200),
      y: vertical.clamp(-1200, 1200),
    );
  }

  @override
  Widget build(BuildContext context) => LayoutBuilder(
    builder: (context, constraints) {
      if (constraints.maxWidth <= 0 || constraints.maxHeight <= 0) {
        return const SizedBox.shrink();
      }
      final width = widget.frameWidth > 0 ? widget.frameWidth : 1280;
      final height = widget.frameHeight > 0 ? widget.frameHeight : 720;
      final ratio = MediaQuery.devicePixelRatioOf(context);
      final layout = remoteFrameLayout(
        constraints.biggest,
        Size(width.toDouble(), height.toDouble()),
        widget.scale,
        ratio,
      );
      final size = layout.size;
      Widget texture = Texture(
        textureId: widget.texture,
        filterQuality: layout.filter,
      );
      if (defaultTargetPlatform == TargetPlatform.macOS) {
        final tw = (size.width * ratio).round(),
            th = (size.height * ratio).round();
        final enlarge =
            widget.scale == SurfaceScale.fit &&
            layout.filter != FilterQuality.none &&
            tw > width &&
            th > height &&
            tw <= 4096 &&
            th <= 4096 &&
            tw * th <= 4096 * 2304;
        texture = _MacVideoTexture(
          texture: widget.texture,
          source: (width, height),
          target: enlarge ? (tw, th) : (0, 0),
          liftShadows: widget.liftShadows,
          filter: layout.filter,
        );
      }
      double snap(double available, double drawn) {
        final centered = math.max(0.0, (available - drawn) / 2);
        return (centered * ratio).round() / ratio - centered;
      }

      final surface = Center(
        child: Transform.translate(
          offset: Offset(
            snap(constraints.maxWidth, size.width),
            snap(constraints.maxHeight, size.height),
          ),
          child: _surface(size, texture),
        ),
      );
      if (size.width <= constraints.maxWidth &&
          size.height <= constraints.maxHeight) {
        return surface;
      }
      return Scrollbar(
        controller: _vertical,
        thumbVisibility: size.height > constraints.maxHeight,
        child: SingleChildScrollView(
          controller: _vertical,
          physics: const NeverScrollableScrollPhysics(),
          child: Scrollbar(
            controller: _horizontal,
            thumbVisibility: size.width > constraints.maxWidth,
            notificationPredicate: (notification) =>
                notification.metrics.axis == Axis.horizontal,
            child: SingleChildScrollView(
              controller: _horizontal,
              physics: const NeverScrollableScrollPhysics(),
              scrollDirection: Axis.horizontal,
              child: ConstrainedBox(
                constraints: BoxConstraints(
                  minWidth: constraints.maxWidth,
                  minHeight: constraints.maxHeight,
                ),
                child: surface,
              ),
            ),
          ),
        ),
      );
    },
  );

  Widget _surface(Size size, Widget texture) => SizedBox.fromSize(
    size: size,
    child: Focus(
      focusNode: widget.focus,
      onKeyEvent: _key,
      onFocusChange: (focused) {
        if (!focused) _release();
      },
      child: MouseRegion(
        cursor: SystemMouseCursors.basic,
        onExit: (_) {
          if (_buttons != 0) _release();
        },
        child: Listener(
          behavior: HitTestBehavior.opaque,
          onPointerDown: (event) {
            if (widget.canInput) widget.focus.requestFocus();
            _pointer(event, size, buttonEvent: true);
          },
          onPointerUp: (event) => _pointer(event, size, buttonEvent: true),
          onPointerCancel: (_) => _release(),
          onPointerMove: (event) => _pointer(event, size),
          onPointerHover: (event) => _pointer(event, size),
          onPointerSignal: (event) {
            if (event is PointerScrollEvent) {
              _wheel(event.localPosition, event.scrollDelta, size);
            }
          },
          onPointerPanZoomUpdate: (event) {
            if (event.scale == 1 && event.rotation == 0) {
              _wheel(event.localPosition, -event.localPanDelta, size);
            }
          },
          child: texture,
        ),
      ),
    ),
  );
}

class _MacVideoTexture extends StatefulWidget {
  const _MacVideoTexture({
    required this.texture,
    required this.source,
    required this.target,
    required this.liftShadows,
    required this.filter,
  });

  final int texture;
  final (int, int) source, target;
  final bool liftShadows;
  final FilterQuality filter;

  @override
  State<_MacVideoTexture> createState() => _MacVideoTextureState();
}

class _MacVideoTextureState extends State<_MacVideoTexture> {
  static const _channel = MethodChannel('dev.nexdesk/m1_texture');
  Timer? _resize;
  int _revision = 0;
  bool _nativeSize = false;

  @override
  void initState() {
    super.initState();
    unawaited(_configure(++_revision));
  }

  @override
  void didUpdateWidget(_MacVideoTexture old) {
    super.didUpdateWidget(old);
    final sameSource =
        widget.texture == old.texture && widget.source == old.source;
    final sameTone = widget.liftShadows == old.liftShadows;
    if (sameSource && sameTone && widget.target == old.target) return;
    final revision = ++_revision;
    _nativeSize = false;
    _resize?.cancel();
    if (sameSource && sameTone && widget.target != (0, 0)) {
      // While dragging, Flutter can scale the previous texture immediately.
      _resize = Timer(
        const Duration(milliseconds: 80),
        () => _configure(revision),
      );
    } else {
      unawaited(_configure(revision));
    }
  }

  Future<void> _configure(int revision) async {
    var accepted = false;
    try {
      accepted =
          await _channel.invokeMethod<bool>('presentation', {
            'texture': widget.texture,
            'sourceWidth': widget.source.$1,
            'sourceHeight': widget.source.$2,
            'targetWidth': widget.target.$1,
            'targetHeight': widget.target.$2,
            'liftShadows': widget.liftShadows,
          }) ==
          true;
    } on MissingPluginException {
      // Older runners still display the original texture.
    } on PlatformException {
      // The texture may have detached while a new layout was pending.
    }
    if (mounted &&
        revision == _revision &&
        accepted &&
        widget.target != (0, 0)) {
      setState(() => _nativeSize = true);
    }
  }

  @override
  void dispose() {
    _resize?.cancel();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => Texture(
    textureId: widget.texture,
    filterQuality: _nativeSize ? FilterQuality.low : widget.filter,
  );
}
