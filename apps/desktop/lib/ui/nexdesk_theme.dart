import 'package:flutter/material.dart';

ThemeData nexdeskTheme(Brightness brightness) {
  final dark = brightness == Brightness.dark;
  final primary = Color(dark ? 0xff9db3ff : 0xff294ed8);
  final ink = Color(dark ? 0xffedf1fa : 0xff161b29);
  final muted = Color(dark ? 0xffaab6cd : 0xff526079);
  final panel = Color(dark ? 0xff1a1f2c : 0xffffffff);
  final ground = Color(dark ? 0xff121620 : 0xfff3f5fa);
  final field = Color(dark ? 0xff222b3d : 0xffedf1f8);
  final edge = Color(dark ? 0xff354158 : 0xffd3dcea);
  final blue = Color(dark ? 0xff3459df : 0xff294ed8);
  final colors = ColorScheme.fromSeed(seedColor: blue, brightness: brightness)
      .copyWith(
        primary: primary,
        onPrimary: dark ? const Color(0xff111a42) : Colors.white,
        primaryContainer: dark
            ? const Color(0xff283a70)
            : const Color(0xffe4ebff),
        onPrimaryContainer: dark
            ? const Color(0xffe1eaff)
            : const Color(0xff203e9d),
        surface: panel,
        onSurface: ink,
        onSurfaceVariant: muted,
        surfaceContainerLowest: panel,
        surfaceContainerLow: ground,
        surfaceContainer: field,
        surfaceContainerHigh: field,
        surfaceContainerHighest: field,
        outline: dark ? const Color(0xff687895) : const Color(0xff8190a9),
        outlineVariant: edge,
      );
  final corner = BorderRadius.circular(7);
  final shape = RoundedRectangleBorder(borderRadius: corner);
  final dialogShape = RoundedRectangleBorder(
    borderRadius: BorderRadius.circular(12),
    side: BorderSide(color: edge),
  );
  final outline = OutlineInputBorder(
    borderRadius: corner,
    borderSide: BorderSide(color: edge),
  );
  final theme = ThemeData(
    colorScheme: colors,
    scaffoldBackgroundColor: ground,
    dividerColor: edge,
    appBarTheme: AppBarThemeData(
      centerTitle: false,
      backgroundColor: panel,
      foregroundColor: ink,
      surfaceTintColor: Colors.transparent,
      elevation: 0,
      scrolledUnderElevation: 0,
    ),
    dialogTheme: DialogThemeData(
      backgroundColor: panel,
      surfaceTintColor: Colors.transparent,
      shape: dialogShape,
      actionsPadding: const EdgeInsets.fromLTRB(20, 0, 20, 18),
    ),
    popupMenuTheme: PopupMenuThemeData(
      color: panel,
      surfaceTintColor: Colors.transparent,
      shape: dialogShape,
    ),
    cardTheme: CardThemeData(
      color: panel,
      surfaceTintColor: Colors.transparent,
      elevation: 0,
      shape: RoundedRectangleBorder(
        borderRadius: BorderRadius.circular(10),
        side: BorderSide(color: edge),
      ),
    ),
    inputDecorationTheme: InputDecorationThemeData(
      filled: true,
      fillColor: field,
      hoverColor: primary.withValues(alpha: .035),
      isDense: true,
      contentPadding: const EdgeInsets.symmetric(horizontal: 12, vertical: 13),
      hintStyle: TextStyle(color: muted, fontSize: 13),
      errorMaxLines: 2,
      border: outline,
      enabledBorder: outline,
      focusedBorder: outline.copyWith(
        borderSide: BorderSide(color: primary, width: 1.3),
      ),
    ),
    filledButtonTheme: FilledButtonThemeData(
      style: ButtonStyle(
        foregroundColor: WidgetStateProperty.resolveWith(
          (states) =>
              states.contains(WidgetState.disabled) ? null : Colors.white,
        ),
        backgroundColor: WidgetStateProperty.resolveWith((states) {
          if (states.contains(WidgetState.disabled)) return null;
          if (states.contains(WidgetState.pressed)) {
            return const Color(0xff1a369e);
          }
          if (states.contains(WidgetState.hovered)) {
            return const Color(0xff2243bf);
          }
          return blue;
        }),
        minimumSize: const WidgetStatePropertyAll(Size(64, 40)),
        shape: WidgetStatePropertyAll(shape),
        textStyle: const WidgetStatePropertyAll(
          TextStyle(fontSize: 13, fontWeight: FontWeight.w700),
        ),
      ),
    ),
    outlinedButtonTheme: OutlinedButtonThemeData(
      style: OutlinedButton.styleFrom(
        foregroundColor: ink,
        minimumSize: const Size(40, 36),
        shape: shape,
        side: BorderSide(color: edge),
      ),
    ),
    textButtonTheme: TextButtonThemeData(
      style: TextButton.styleFrom(shape: shape),
    ),
    snackBarTheme: SnackBarThemeData(
      behavior: SnackBarBehavior.floating,
      backgroundColor: const Color(0xff20283b),
      contentTextStyle: const TextStyle(color: Colors.white),
      shape: shape,
    ),
  );
  return theme.copyWith(
    textTheme: theme.textTheme.copyWith(
      titleLarge: TextStyle(
        color: ink,
        fontSize: 21,
        fontWeight: FontWeight.w700,
      ),
      titleMedium: TextStyle(
        color: ink,
        fontSize: 16,
        fontWeight: FontWeight.w700,
      ),
      bodyMedium: TextStyle(color: ink, fontSize: 13, height: 1.45),
      bodySmall: TextStyle(color: muted, fontSize: 12, height: 1.45),
      labelLarge: TextStyle(
        color: ink,
        fontSize: 13,
        fontWeight: FontWeight.w600,
      ),
      labelMedium: TextStyle(
        color: muted,
        fontSize: 12,
        fontWeight: FontWeight.w600,
      ),
    ),
  );
}

class NexDeskMark extends StatelessWidget {
  const NexDeskMark({super.key, this.size = 34});

  final double size;

  @override
  Widget build(BuildContext context) => Container(
    width: size,
    height: size,
    decoration: BoxDecoration(
      color: const Color(0xff182544),
      borderRadius: BorderRadius.circular(7),
    ),
    child: Icon(
      Icons.desktop_windows_outlined,
      size: size * .57,
      color: const Color(0xffced9ff),
    ),
  );
}
