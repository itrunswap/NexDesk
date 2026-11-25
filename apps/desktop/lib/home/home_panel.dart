import 'package:flutter/material.dart';

class HomePanel extends StatelessWidget {
  const HomePanel({
    super.key,
    required this.title,
    required this.children,
    this.subtitle,
    this.action,
  });

  final String title;
  final String? subtitle;
  final List<Widget> children;
  final Widget? action;

  @override
  Widget build(BuildContext context) {
    final text = Theme.of(context).textTheme;
    return Card(
      margin: EdgeInsets.zero,
      child: Padding(
        padding: const EdgeInsets.all(15),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.stretch,
          mainAxisAlignment: MainAxisAlignment.spaceBetween,
          children: [
            Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              mainAxisSize: MainAxisSize.min,
              children: [
                Text(title, style: text.titleMedium),
                if (subtitle case final description?) ...[
                  const SizedBox(height: 3),
                  Text(description, style: text.bodySmall),
                ],
                const SizedBox(height: 13),
                ...children,
              ],
            ),
            if (action case final button?)
              Padding(padding: const EdgeInsets.only(top: 14), child: button),
          ],
        ),
      ),
    );
  }
}
