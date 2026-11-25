import 'package:flutter/material.dart';

import 'native_collaboration.dart';

class CapabilityChoices extends StatelessWidget {
  const CapabilityChoices({
    super.key,
    required this.available,
    required this.selected,
    required this.onChanged,
  });

  final int available;
  final int selected;
  final ValueChanged<int> onChanged;

  @override
  Widget build(BuildContext context) => Column(
    mainAxisSize: MainAxisSize.min,
    children: [
      for (final capability in CollaborationCapability.values)
        if (available & capability.bit != 0)
          CheckboxListTile(
            dense: true,
            contentPadding: EdgeInsets.zero,
            controlAffinity: ListTileControlAffinity.leading,
            title: Text(capability.label),
            value: selected & capability.bit != 0,
            onChanged: (checked) => onChanged(
              checked == true
                  ? selected | capability.bit
                  : selected & ~capability.bit,
            ),
          ),
    ],
  );
}
