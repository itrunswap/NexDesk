import 'dart:io';

const defaultDirectoryOrigin = String.fromEnvironment(
  'NEXDESK_DEFAULT_DIRECTORY',
  defaultValue: 'https://desk.zoukaixin.cn',
);

bool validDirectoryOrigin(String value) {
  if (value.length > 512) return false;
  final origin = Uri.tryParse(value);
  if (origin == null ||
      origin.host.isEmpty ||
      origin.userInfo.isNotEmpty ||
      origin.hasQuery ||
      origin.hasFragment ||
      (origin.path.isNotEmpty && origin.path != '/') ||
      origin.port < 1 ||
      origin.port > 65535) {
    return false;
  }
  return origin.scheme == 'https' ||
      (origin.scheme == 'http' &&
          (origin.host == 'localhost' ||
              origin.host == '127.0.0.1' ||
              origin.host == '::1'));
}

String? normalizeDirectoryOrigin(String input) {
  final value = input.trim();
  return validDirectoryOrigin(value) ? Uri.parse(value).origin : null;
}

String? directoryOriginFromFields(String address, String port) {
  final host = address.trim();
  if (host.isEmpty) return null;
  final ipv6 = InternetAddress.tryParse(host)?.type == InternetAddressType.IPv6;
  final value = host.contains('://')
      ? host
      : (ipv6 ? 'https://[$host]' : 'https://$host');
  final origin = normalizeDirectoryOrigin(value);
  if (origin == null) return null;
  final text = port.trim();
  if (text.isEmpty) return origin;
  final selected = int.tryParse(text);
  if (selected == null ||
      !RegExp(r'^[0-9]+$').hasMatch(text) ||
      selected < 1 ||
      selected > 65535) {
    return null;
  }
  final uri = Uri.parse(origin);
  final explicit = RegExp(r':([0-9]+)/?$').firstMatch(value)?[1];
  if (explicit != null && int.tryParse(explicit) != selected) return null;
  return uri.replace(port: selected).origin;
}
