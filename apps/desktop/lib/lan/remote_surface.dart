import 'dart:math' as math;

import 'package:flutter/gestures.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import 'native_desktop.dart';

enum SurfaceScale { fit, original, enlarge }

class RemoteSurface extends StatefulWidget {
  const RemoteSurface({
    super.key,
    required this.native,
    required this.viewer,
    required this.texture,
    required this.focus,
    required this.frameWidth,
    required this.frameHeight,
    required this.generation,
    required this.canInput,
    this.scale = SurfaceScale.fit,
  });

  final NativeDesktop native;
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
    widget.native.send(widget.viewer, DesktopAction.release);
  }

  KeyEventResult _key(FocusNode _, KeyEvent event) {
    if (!widget.canInput) return KeyEventResult.ignored;
    final usage = event.physicalKey.usbHidUsage;
    if (usage < 0x00070004 || usage > 0x000700e7) return KeyEventResult.ignored;
    widget.native.send(
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
          widget.native.send(
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
    widget.native.send(
      widget.viewer,
      DesktopAction.move,
      generation: widget.generation,
      x: x,
      y: y,
    );
  }

  void _wheel(PointerSignalEvent event, Size size) {
    if (!widget.canInput || event is! PointerScrollEvent || size.isEmpty) {
      return;
    }
    final (x, y) = _point(event.localPosition, size);
    widget.native.send(
      widget.viewer,
      DesktopAction.move,
      generation: widget.generation,
      x: x,
      y: y,
    );
    int delta(double value) => (value * 3).round().clamp(-1200, 1200);
    widget.native.send(
      widget.viewer,
      DesktopAction.wheel,
      generation: widget.generation,
      x: delta(event.scrollDelta.dx),
      y: delta(-event.scrollDelta.dy),
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
      final fit = math.min(
        constraints.maxWidth / width,
        constraints.maxHeight / height,
      );
      final factor = switch (widget.scale) {
        SurfaceScale.fit => fit,
        SurfaceScale.original => 1 / View.of(context).devicePixelRatio,
        SurfaceScale.enlarge => 2 / View.of(context).devicePixelRatio,
      };
      final size = Size(width * factor, height * factor);
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
        cursor: widget.canInput
            ? SystemMouseCursors.precise
            : SystemMouseCursors.basic,
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
          onPointerSignal: (event) => _wheel(event, size),
          child: Texture(textureId: widget.texture),
        ),
      ),
    ),
  );
}
