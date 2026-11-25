import 'dart:async';
import 'dart:convert';
import 'dart:ffi' as ffi;

import 'package:ffi/ffi.dart';

enum CollaborationCapability {
  audio(1, '系统声音'),
  clipboard(2, '文本剪贴板'),
  fileRead(4, '浏览并下载文件'),
  fileWrite(8, '浏览并上传文件'),
  display(16, '修改分辨率'),
  text(32, '输入已提交文本');

  const CollaborationCapability(this.bit, this.label);
  final int bit;
  final String label;
}

typedef M3Connect = int Function(
  ffi.Pointer<ffi.Uint8>,
  int,
  int,
  ffi.Pointer<ffi.Uint8>,
  int,
  int,
  int,
);
typedef _Submit = int Function(int, ffi.Pointer<ffi.Uint8>, int);
typedef _Reply = int Function(int, int, ffi.Pointer<ffi.Uint8>, int);
typedef _Info = int Function(int, ffi.Pointer<ffi.Uint8>, int);

final class NativeCollaboration {
  NativeCollaboration._(ffi.DynamicLibrary lib)
    : connect = lib
          .lookupFunction<
            ffi.Uint64 Function(
              ffi.Pointer<ffi.Uint8>,
              ffi.Size,
              ffi.Uint16,
              ffi.Pointer<ffi.Uint8>,
              ffi.Size,
              ffi.Uint32,
              ffi.Uint32,
            ),
            M3Connect
          >('nexdesk_m3_connect'),
      _submit = lib
          .lookupFunction<
            ffi.Uint64 Function(ffi.Uint64, ffi.Pointer<ffi.Uint8>, ffi.Size),
            _Submit
          >('nexdesk_m3_request'),
      _reply = lib
          .lookupFunction<
            ffi.Size Function(
              ffi.Uint64,
              ffi.Uint64,
              ffi.Pointer<ffi.Uint8>,
              ffi.Size,
            ),
            _Reply
          >('nexdesk_m3_poll'),
      _info = lib
          .lookupFunction<
            ffi.Size Function(ffi.Uint64, ffi.Pointer<ffi.Uint8>, ffi.Size),
            _Info
          >('nexdesk_m3_info');

  static NativeCollaboration? tryLoad(ffi.DynamicLibrary library) {
    try {
      return NativeCollaboration._(library);
    } on ArgumentError {
      return null;
    }
  }

  final M3Connect connect;
  final _Submit _submit;
  final _Reply _reply;
  final _Info _info;

  Map<String, Object?> inspect(int viewer) =>
      _read((output, length) => _info(viewer, output, length)) ?? const {};

  Future<Map<String, Object?>> request(
    int viewer,
    Map<String, Object?> request,
  ) async {
    final data = utf8.encode(jsonEncode(request));
    if (data.length > 12 * 1024) throw const CollaborationIssue('limit');
    final marker = using((arena) {
      final input = arena<ffi.Uint8>(data.length);
      input.asTypedList(data.length).setAll(0, data);
      return _submit(viewer, input, data.length);
    });
    if (marker == 0) throw const CollaborationIssue('disconnected');
    final timer = Stopwatch()..start();
    while (timer.elapsed < const Duration(seconds: 10)) {
      final reply = _read(
        (buffer, length) => _reply(viewer, marker, buffer, length),
      );
      if (reply != null) {
        if (reply['error'] case final String code) {
          throw CollaborationIssue(code);
        }
        return reply;
      }
      await Future<void>.delayed(const Duration(milliseconds: 45));
    }
    throw const CollaborationIssue('timeout');
  }

  Future<Map<String, Object?>> remote(
    int viewer,
    String kind, {
    Map<String, Object?> fields = const {},
    bool remember = false,
  }) => request(viewer, {
    'operation': 'remote',
    'request': {'kind': kind, ...fields},
    if (remember) 'remember': true,
  });

  Map<String, Object?>? _read(int Function(ffi.Pointer<ffi.Uint8>, int) fill) =>
      using((arena) {
        const capacity = 32 * 1024;
        final bytes = arena<ffi.Uint8>(capacity);
        final size = fill(bytes, capacity);
        if (size == 0) return null;
        return (jsonDecode(utf8.decode(bytes.asTypedList(size))) as Map)
            .cast<String, Object?>();
      });
}

class CollaborationIssue implements Exception {
  const CollaborationIssue(this.code);
  final String code;

  String get message => switch (code) {
    'denied' => '本次会话没有授予该权限。',
    'unavailable' => '系统暂时无法完成此操作。',
    'invalid' => '输入无效，或文本超出会话限制。',
    'limit' => '所选内容超过单次操作的限制。',
    'missing' => '该文件或目录已不存在。',
    'changed' => '来源或目标已变化，请刷新后重试。',
    'busy' => '操作尚未就绪，请稍候。',
    'expired' => '确认已超时，请重新选择显示模式。',
    'disconnected' => '协作通道未连接。',
    'conflict' => '目标位置有不兼容的同名项目。',
    'canceled' => '已取消。',
    'full' => '目标磁盘空间不足。',
    'storage' => '本机无法保存设置。',
    'timeout' => '等待操作结果超时。',
    _ => '文件或会话操作失败。',
  };
}
