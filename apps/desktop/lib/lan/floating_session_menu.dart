import 'dart:math' as math;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

class FloatingSessionMenu extends StatelessWidget {
  const FloatingSessionMenu({
    super.key,
    required this.expanded,
    required this.onToggle,
    required this.surface,
    required this.controls,
  });

  final bool expanded;
  final VoidCallback onToggle;
  final Widget surface, controls;

  @override
  Widget build(BuildContext context) {
    final colors = Theme.of(context).colorScheme;
    return CallbackShortcuts(
      bindings: {
        if (expanded)
          const SingleActivator(LogicalKeyboardKey.escape): onToggle,
      },
      child: LayoutBuilder(
        builder: (context, bounds) => Stack(
          fit: StackFit.expand,
          children: [
            surface,
            if (expanded)
              ModalBarrier(
                dismissible: true,
                onDismiss: onToggle,
                semanticsLabel: '收起会话操作',
              ),
            Positioned(
              top: 61,
              right: 12,
              child: Visibility(
                visible: expanded,
                maintainState: true,
                child: ExcludeFocus(
                  excluding: !expanded,
                  child: ConstrainedBox(
                    constraints: BoxConstraints(
                      maxWidth: math.min(
                        408,
                        math.max(0, bounds.maxWidth - 24),
                      ),
                      maxHeight: math.max(0, bounds.maxHeight - 73),
                    ),
                    child: Material(
                      color: colors.surface,
                      elevation: 8,
                      clipBehavior: Clip.antiAlias,
                      shape: RoundedRectangleBorder(
                        side: BorderSide(color: colors.outlineVariant),
                        borderRadius: BorderRadius.circular(10),
                      ),
                      child: SingleChildScrollView(
                        padding: const EdgeInsets.all(12),
                        child: controls,
                      ),
                    ),
                  ),
                ),
              ),
            ),
            Positioned(
              right: 12,
              top: 12,
              child: SizedBox.square(
                dimension: 42,
                child: IconButton.filled(
                  tooltip: expanded ? '收起会话操作' : '会话操作',
                  onPressed: onToggle,
                  style: IconButton.styleFrom(
                    foregroundColor: expanded
                        ? colors.onPrimary
                        : colors.onSurface,
                    backgroundColor: expanded ? colors.primary : colors.surface,
                    padding: EdgeInsets.zero,
                    shape: RoundedRectangleBorder(
                      borderRadius: BorderRadius.circular(9),
                      side: BorderSide(color: colors.outlineVariant),
                    ),
                  ),
                  icon: Icon(
                    expanded ? Icons.close_rounded : Icons.tune_rounded,
                    size: 21,
                  ),
                ),
              ),
            ),
          ],
        ),
      ),
    );
  }
}
