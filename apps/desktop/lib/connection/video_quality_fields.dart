import 'package:flutter/material.dart';

import 'video_quality.dart';

class VideoQualityFields extends StatelessWidget {
  const VideoQualityFields({
    super.key,
    required this.value,
    required this.onChanged,
  });

  final VideoQuality value;
  final ValueChanged<VideoQuality> onChanged;

  @override
  Widget build(BuildContext context) {
    final rates = {10, 15, 24, 30, 45, 60, value.fps}.toList()..sort();
    final bandwidths = {
      6,
      8,
      12,
      16,
      24,
      32,
      48,
      64,
      80,
      ?value.maxMbps,
    }.toList()..sort();
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      mainAxisSize: MainAxisSize.min,
      children: [
        _select('画质模式', value.profile, {
          for (final profile in VideoProfile.values) profile: profile.label,
        }, (profile) => onChanged(value.withProfile(profile))),
        const SizedBox(height: 14),
        LayoutBuilder(
          builder: (context, limits) {
            final resolution = _select(
              '传输分辨率上限',
              value.resolution,
              {
                for (final resolution in StreamResolution.values)
                  resolution: resolution.label,
              },
              (resolution) => onChanged(value.withCaps(resolution: resolution)),
            );
            final fps = _select('帧率上限', value.fps, {
              for (final fps in rates) fps: '$fps FPS',
            }, (fps) => onChanged(value.withCaps(fps: fps)));
            if (limits.maxWidth < 330) {
              return Column(
                children: [resolution, const SizedBox(height: 14), fps],
              );
            }
            return Row(
              children: [
                Expanded(flex: 3, child: resolution),
                const SizedBox(width: 12),
                Expanded(flex: 2, child: fps),
              ],
            );
          },
        ),
        const SizedBox(height: 14),
        _select('视频码率上限', value.maxMbps ?? 0, {
          0: '自动按画质模式',
          for (final rate in bandwidths) rate: '$rate Mbps',
        }, (rate) => onChanged(value.withMaxMbps(rate == 0 ? null : rate))),
        const SizedBox(height: 8),
        Text(
          '分辨率上限只调整传输画面。实际画质随网络和两端性能调整。',
          style: Theme.of(context).textTheme.bodySmall,
        ),
      ],
    );
  }

  Widget _select<T>(
    String label,
    T selected,
    Map<T, String> options,
    ValueChanged<T> onSelected,
  ) => InputDecorator(
    decoration: InputDecoration(labelText: label),
    child: DropdownButtonHideUnderline(
      child: DropdownButton<T>(
        value: selected,
        isDense: true,
        isExpanded: true,
        items: [
          for (final item in options.entries)
            DropdownMenuItem(
              value: item.key,
              child: Text(item.value, overflow: TextOverflow.ellipsis),
            ),
        ],
        onChanged: (value) {
          if (value != null) onSelected(value);
        },
      ),
    ),
  );
}
