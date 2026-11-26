import 'dart:math' as math;

import 'package:flutter/gestures.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import 'native_desktop.dart';

enum SurfaceScale { fit, original, enlarge }

Size remoteFrameLayout(
  Size viewport,
  Size frame,
  SurfaceScale scale,
  double pixelRatio,
) {
  final factor = switch (scale) {
    SurfaceScale.fit => math.min(
      viewport.width / frame.width,
      viewport.height / frame.height,
    ),
    SurfaceScale.original => 1 / pixelRatio,
    SurfaceScale.enlarge => 2 / pixelRatio,
  };
  return frame * factor;
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
  });

  final DesktopActionSender send;
  final int viewer, texture, frameWidth, frameHeight, generation;
  final FocusNode focus;
  final bool canInput;
  final SurfaceScale scale;

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
      final size = remoteFrameLayout(
        constraints.biggest,
        Size(width.toDouble(), height.toDouble()),
        widget.scale,
        View.of(context).devicePixelRatio,
      );
      final surface = Center(child: _surface(size));
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

  Widget _surface(Size size) => SizedBox.fromSize(
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
          child: Texture(
            textureId: widget.texture,
            filterQuality: switch (widget.scale) {
              SurfaceScale.original => FilterQuality.none,
              SurfaceScale.fit => FilterQuality.medium,
              SurfaceScale.enlarge => FilterQuality.low,
            },
          ),
        ),
      ),
    ),
  );
}
