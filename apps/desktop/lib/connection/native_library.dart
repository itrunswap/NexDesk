import 'dart:io';

String? locateCoreLibrary() {
  const configured = String.fromEnvironment('NEXDESK_CORE_LIBRARY');
  if (configured.isNotEmpty) return configured;
  final name = Platform.isWindows
      ? 'nexdesk_ffi.dll'
      : Platform.isMacOS
      ? 'libnexdesk_ffi.dylib'
      : Platform.isLinux
      ? 'libnexdesk_ffi.so'
      : null;
  if (name == null) return null;
  final path =
      '${File(Platform.resolvedExecutable).parent.path}${Platform.pathSeparator}$name';
  return File(path).existsSync() ? path : null;
}
