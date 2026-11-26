import 'dart:math' as math;

import 'package:flutter/material.dart';

import '../lan/native_desktop.dart';

enum RecentAction { rename, group, presence, disconnect, forget, remove }

class RecentDeviceGrid extends StatelessWidget {
  const RecentDeviceGrid({
    super.key,
    required this.devices,
    required this.onConnect,
    required this.onAction,
    required this.isOpen,
    required this.presence,
    required this.enabled,
    this.showOrigin = false,
  });

  final List<RecentDevice> devices;
  final ValueChanged<RecentDevice> onConnect;
  final void Function(RecentDevice, RecentAction) onAction;
  final bool Function(RecentDevice) isOpen;
  final String? Function(RecentDevice) presence;
  final bool enabled, showOrigin;

  @override
  Widget build(BuildContext context) => LayoutBuilder(
    builder: (context, bounds) {
      const gap = 10.0;
      final columns = ((bounds.maxWidth + gap) / 198).floor().clamp(1, 3);
      final width = math.max(
        0.0,
        (bounds.maxWidth - (columns - 1) * gap) / columns,
      );
      return Wrap(
        spacing: gap,
        runSpacing: gap,
        children: [
          for (final device in devices)
            SizedBox(
              width: width,
              child: _RecentDeviceCard(
                device: device,
                open: isOpen(device),
                status: presence(device),
                showOrigin: showOrigin,
                onConnect: enabled ? () => onConnect(device) : null,
                onAction: (action) => onAction(device, action),
              ),
            ),
        ],
      );
    },
  );
}

class _RecentDeviceCard extends StatelessWidget {
  const _RecentDeviceCard({
    required this.device,
    required this.open,
    required this.status,
    required this.showOrigin,
    required this.onConnect,
    required this.onAction,
  });

  final RecentDevice device;
  final bool open, showOrigin;
  final String? status;
  final VoidCallback? onConnect;
  final ValueChanged<RecentAction> onAction;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final colors = theme.colorScheme;
    final endpoint = showOrigin || device.origin == null
        ? device.endpoint
        : device.address;
    return Material(
      color: colors.surfaceContainerLow,
      shape: RoundedRectangleBorder(
        side: BorderSide(color: colors.outlineVariant),
        borderRadius: BorderRadius.circular(9),
      ),
      clipBehavior: Clip.antiAlias,
      child: InkWell(
        onTap: onConnect,
        child: Padding(
          padding: const EdgeInsets.fromLTRB(10, 6, 6, 8),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Row(
                children: [
                  Icon(
                    Icons.desktop_windows_outlined,
                    size: 19,
                    color: colors.primary,
                  ),
                  const SizedBox(width: 6),
                  Expanded(
                    child: Text(
                      open
                          ? '窗口已打开'
                          : status ??
                                (device.origin == null ? 'IP 直连' : '设备 ID'),
                      style: theme.textTheme.labelSmall?.copyWith(
                        color: open ? colors.primary : colors.onSurfaceVariant,
                      ),
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                    ),
                  ),
                  PopupMenuButton<RecentAction>(
                    tooltip: '管理近期设备',
                    padding: EdgeInsets.zero,
                    icon: const Icon(Icons.more_horiz_rounded, size: 19),
                    onSelected: onAction,
                    itemBuilder: (_) => [
                      if (open)
                        const PopupMenuItem(
                          value: RecentAction.disconnect,
                          child: Text('断开会话'),
                        ),
                      const PopupMenuItem(
                        value: RecentAction.rename,
                        child: Text('编辑别名'),
                      ),
                      const PopupMenuItem(
                        value: RecentAction.group,
                        child: Text('修改分组'),
                      ),
                      if (device.origin != null)
                        const PopupMenuItem(
                          value: RecentAction.presence,
                          child: Text('检查在线状态'),
                        ),
                      if (device.remembered)
                        const PopupMenuItem(
                          value: RecentAction.forget,
                          child: Text('清除保存的密码'),
                        ),
                      const PopupMenuItem(
                        value: RecentAction.remove,
                        child: Text('移除记录'),
                      ),
                    ],
                  ),
                ],
              ),
              Row(
                children: [
                  Expanded(
                    child: Column(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        Text(
                          device.label,
                          style: theme.textTheme.labelLarge,
                          maxLines: 1,
                          overflow: TextOverflow.ellipsis,
                        ),
                        Text(
                          endpoint,
                          style: theme.textTheme.bodySmall,
                          maxLines: 1,
                          overflow: TextOverflow.ellipsis,
                        ),
                      ],
                    ),
                  ),
                  const SizedBox(width: 4),
                  IconButton.filledTonal(
                    tooltip: open ? '返回会话窗口' : '连接设备',
                    onPressed: onConnect,
                    constraints: const BoxConstraints.tightFor(
                      width: 32,
                      height: 32,
                    ),
                    style: IconButton.styleFrom(
                      padding: EdgeInsets.zero,
                      tapTargetSize: MaterialTapTargetSize.shrinkWrap,
                    ),
                    icon: Icon(
                      open
                          ? Icons.open_in_new_rounded
                          : Icons.arrow_forward_rounded,
                      size: 17,
                    ),
                  ),
                ],
              ),
              if (device.group.isNotEmpty)
                Text(
                  device.group,
                  style: theme.textTheme.labelSmall,
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                ),
            ],
          ),
        ),
      ),
    );
  }
}
