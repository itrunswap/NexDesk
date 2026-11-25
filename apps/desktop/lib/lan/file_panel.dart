import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../preview/native_preview.dart';
import 'native_collaboration.dart';

class FilePanel extends StatefulWidget {
  const FilePanel({
    super.key,
    required this.native,
    required this.viewer,
    required this.capabilities,
    this.initialDirectory = '',
  });

  final NativeCollaboration native;
  final int viewer;
  final int capabilities;
  final String initialDirectory;

  @override
  State<FilePanel> createState() => _FilePanelState();
}

class _FilePanelState extends State<FilePanel> {
  final _local = _Browser(false);
  final _remote = _Browser(true);
  final _root = TextEditingController();
  Timer? _timer;
  List<Map<String, Object?>> _tasks = const [];
  String? _issue;
  bool _rootOpen = false;

  bool get _upload =>
      widget.capabilities & CollaborationCapability.fileWrite.bit != 0;
  bool get _download =>
      widget.capabilities & CollaborationCapability.fileRead.bit != 0;

  @override
  void initState() {
    super.initState();
    final info = widget.native.inspect(widget.viewer);
    if (info['local_root'] case final String path) {
      _rootOpen = true;
      _root.text = path;
      unawaited(_open(_local, const []));
    } else if (widget.initialDirectory.isNotEmpty) {
      _root.text = widget.initialDirectory;
      unawaited(_chooseRoot());
    }
    unawaited(_open(_remote, const []));
    _timer = Timer.periodic(const Duration(milliseconds: 400), (_) {
      final tasks = _maps(widget.native.inspect(widget.viewer)['tasks']);
      final wasBusy = _tasks.any(_busy);
      setState(() => _tasks = tasks);
      if (wasBusy && !tasks.any(_busy)) {
        if (_rootOpen && !_local.loading) unawaited(_open(_local, _local.path));
        if (!_remote.loading) unawaited(_open(_remote, _remote.path));
      }
    });
  }

  @override
  void dispose() {
    _timer?.cancel();
    _root.dispose();
    super.dispose();
  }

  static bool _busy(Map<String, Object?> task) =>
      !const {'complete', 'failed', 'canceled'}.contains(task['phase']);

  Future<void> _chooseRoot() async {
    if (_root.text.trim().isEmpty) return;
    try {
      await widget.native.request(widget.viewer, {
        'operation': 'local_root',
        'path': _root.text.trim(),
      });
      if (!mounted) return;
      setState(() {
        _rootOpen = true;
        _issue = null;
      });
      await _open(_local, const []);
    } on CollaborationIssue catch (error) {
      if (mounted) setState(() => _issue = error.message);
    }
  }

  Future<void> _pickRoot() async {
    try {
      final path = await NativePreview.selectDirectory();
      if (!mounted || path == null) return;
      _root.text = path;
      await _chooseRoot();
    } on PlatformException {
      if (mounted) setState(() => _issue = '目录选择器不可用，请填写完整路径。');
    } on MissingPluginException {
      if (mounted) setState(() => _issue = '目录选择器不可用，请填写完整路径。');
    }
  }

  Future<void> _open(
    _Browser side,
    List<String> path, {
    bool more = false,
  }) async {
    if (side.loading || !mounted) return;
    setState(() {
      side.loading = true;
      side.issue = null;
    });
    try {
      final reply = await widget.native.request(widget.viewer, {
        'operation': 'list',
        'remote': side.remote,
        'path': path,
        'after': more ? side.cursor : null,
      });
      if (!mounted) return;
      final page = _map(reply['ok']);
      setState(() {
        if (!more) {
          side.entries.clear();
          side.selected.clear();
        }
        side.path = List.of(path);
        side.entries.addAll(_maps(page['entries']).map(_FileEntry.fromJson));
        side.sortEntries();
        side.cursor = page['next'] as String?;
      });
    } on CollaborationIssue catch (error) {
      if (mounted) setState(() => side.issue = error.message);
    } finally {
      if (mounted) setState(() => side.loading = false);
    }
  }

  Future<void> _send(_Dragged chosen, _Browser target) async {
    if (!_rootOpen || chosen.items.isEmpty || chosen.remote == target.remote) {
      return;
    }
    if (chosen.remote ? !_download : !_upload) return;
    final destination = List<String>.of(target.path);
    var policy = 'keep_both';
    final accepted = await showDialog<bool>(
      context: context,
      builder: (context) => StatefulBuilder(
        builder: (context, select) => AlertDialog(
          title: Text(chosen.remote ? '下载到主控端' : '上传到被控端'),
          content: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text('${chosen.items.length} 项'),
              Text(
                '目标目录  ${_relative(destination)}',
                overflow: TextOverflow.ellipsis,
              ),
              const SizedBox(height: 12),
              DropdownButtonFormField<String>(
                initialValue: policy,
                decoration: const InputDecoration(labelText: '遇到同名项目'),
                items: const [
                  DropdownMenuItem(value: 'keep_both', child: Text('保留两份')),
                  DropdownMenuItem(value: 'skip', child: Text('跳过')),
                  DropdownMenuItem(
                    value: 'replace',
                    child: Text('替换同名文件，合并同名目录'),
                  ),
                ],
                onChanged: (value) =>
                    select(() => policy = value ?? 'keep_both'),
              ),
            ],
          ),
          actions: [
            TextButton(
              onPressed: () => Navigator.pop(context, false),
              child: const Text('取消'),
            ),
            FilledButton(
              onPressed: () => Navigator.pop(context, true),
              child: const Text('开始传输'),
            ),
          ],
        ),
      ),
    );
    if (!mounted || accepted != true) return;
    await _command({
      'operation': 'transfer',
      'transfer': {
        'direction': chosen.remote ? 'download' : 'upload',
        'source': chosen.path,
        'destination': destination,
        'items': chosen.items,
        'conflict': policy,
      },
    });
  }

  Future<void> _command(Map<String, Object?> fields) async {
    try {
      await widget.native.request(widget.viewer, fields);
      if (mounted) setState(() => _issue = null);
    } on CollaborationIssue catch (error) {
      if (mounted) setState(() => _issue = error.message);
    }
  }

  _Dragged _selection(_Browser side) =>
      _Dragged(side.remote, List.of(side.path), side.selected.toList());

  @override
  Widget build(BuildContext context) => Dialog(
    insetPadding: const EdgeInsets.all(20),
    child: SizedBox(
      width: 1040,
      height: 690,
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          ListTile(
            title: const Text('文件传输'),
            subtitle: const Text('选择项目后发送，或拖入另一侧当前目录。'),
            trailing: IconButton(
              tooltip: '关闭文件面板',
              icon: const Icon(Icons.close),
              onPressed: () => Navigator.pop(context),
            ),
          ),
          Padding(
            padding: const EdgeInsets.symmetric(horizontal: 16),
            child: Row(
              children: [
                Expanded(
                  child: TextField(
                    controller: _root,
                    decoration: InputDecoration(
                      labelText: '主控端目录完整路径',
                      suffixIcon: IconButton(
                        tooltip: '选择主控端目录',
                        icon: const Icon(Icons.folder_open_outlined),
                        onPressed: _tasks.any(_busy) ? null : _pickRoot,
                      ),
                    ),
                    onSubmitted: (_) => _chooseRoot(),
                    enabled: !_tasks.any(_busy),
                  ),
                ),
                const SizedBox(width: 8),
                OutlinedButton(
                  onPressed: _tasks.any(_busy) ? null : _chooseRoot,
                  child: const Text('打开目录'),
                ),
              ],
            ),
          ),
          if (_issue case final issue?)
            Padding(
              padding: const EdgeInsets.fromLTRB(16, 8, 16, 0),
              child: Text(issue),
            ),
          Expanded(
            child: Padding(
              padding: const EdgeInsets.all(16),
              child: LayoutBuilder(
                builder: (context, box) {
                  final left = Expanded(child: _pane(_local));
                  final right = Expanded(child: _pane(_remote));
                  return box.maxWidth >= 610
                      ? Row(
                          crossAxisAlignment: CrossAxisAlignment.stretch,
                          children: [left, const SizedBox(width: 12), right],
                        )
                      : Column(
                          crossAxisAlignment: CrossAxisAlignment.stretch,
                          children: [left, const SizedBox(height: 8), right],
                        );
                },
              ),
            ),
          ),
          if (_tasks.isNotEmpty)
            SizedBox(
              height: 116,
              child: ListView(
                padding: const EdgeInsets.only(bottom: 8),
                children: _tasks.reversed.map(_taskRow).toList(),
              ),
            ),
        ],
      ),
    ),
  );

  Widget _pane(_Browser side) => DragTarget<_Dragged>(
    onWillAcceptWithDetails: (details) =>
        _rootOpen &&
        details.data.remote != side.remote &&
        (side.remote ? _upload : _download),
    onAcceptWithDetails: (details) => _send(details.data, side),
    builder: (context, candidates, _) {
      final color = Theme.of(context).colorScheme;
      final canSend = side.remote ? _download : _upload;
      return Material(
        color: candidates.isEmpty ? color.surface : color.primaryContainer,
        shape: RoundedRectangleBorder(
          borderRadius: BorderRadius.circular(12),
          side: BorderSide(color: color.outlineVariant),
        ),
        clipBehavior: Clip.antiAlias,
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            ListTile(
              dense: true,
              title: Text(side.remote ? '被控端' : '主控端'),
              subtitle: Text(
                _relative(side.path),
                overflow: TextOverflow.ellipsis,
              ),
              trailing: Wrap(
                spacing: 2,
                children: [
                  IconButton(
                    tooltip: '上一级',
                    onPressed: side.path.isEmpty || side.loading
                        ? null
                        : () => _open(
                            side,
                            side.path.sublist(0, side.path.length - 1),
                          ),
                    icon: const Icon(Icons.arrow_upward),
                  ),
                  IconButton(
                    tooltip: '刷新',
                    onPressed: side.loading || !side.remote && !_rootOpen
                        ? null
                        : () => _open(side, side.path),
                    icon: const Icon(Icons.refresh),
                  ),
                  PopupMenuButton<bool>(
                    tooltip: '排列项目',
                    icon: const Icon(Icons.sort),
                    initialValue: side.bySize,
                    onSelected: (value) => setState(() {
                      side.bySize = value;
                      side.sortEntries();
                    }),
                    itemBuilder: (context) => const [
                      PopupMenuItem(value: false, child: Text('按名称')),
                      PopupMenuItem(value: true, child: Text('按大小')),
                    ],
                  ),
                ],
              ),
            ),
            if (side.loading) const LinearProgressIndicator(),
            if (side.issue case final issue?)
              Padding(
                padding: const EdgeInsets.symmetric(horizontal: 12),
                child: Text(issue),
              ),
            Expanded(
              child: ListView.builder(
                itemCount: side.entries.length + (side.cursor == null ? 0 : 1),
                itemBuilder: (context, index) {
                  if (index == side.entries.length) {
                    return TextButton(
                      onPressed: side.loading
                          ? null
                          : () => _open(side, side.path, more: true),
                      child: const Text('加载更多'),
                    );
                  }
                  return _entry(side, side.entries[index], canSend);
                },
              ),
            ),
            Padding(
              padding: const EdgeInsets.all(8),
              child: Align(
                alignment: Alignment.centerRight,
                child: OutlinedButton(
                  onPressed: canSend && _rootOpen && side.selected.isNotEmpty
                      ? () => _send(
                          _selection(side),
                          side.remote ? _local : _remote,
                        )
                      : null,
                  child: Text(side.remote ? '下载所选项目' : '上传所选项目'),
                ),
              ),
            ),
          ],
        ),
      );
    },
  );

  Widget _entry(_Browser side, _FileEntry entry, bool canSend) {
    final selected = side.selected.contains(entry.name);
    final row = ListTile(
      dense: true,
      leading: Icon(
        entry.folder ? Icons.folder_outlined : Icons.insert_drive_file_outlined,
      ),
      title: Text(entry.name, overflow: TextOverflow.ellipsis),
      subtitle: entry.folder ? null : Text(_size(entry.size)),
      onTap: entry.folder
          ? () => _open(side, [...side.path, entry.name])
          : canSend
          ? () => setState(
              () => selected
                  ? side.selected.remove(entry.name)
                  : side.selected.add(entry.name),
            )
          : null,
      trailing: canSend
          ? Checkbox(
              value: selected,
              onChanged: (value) => setState(() {
                if (value == true) {
                  side.selected.add(entry.name);
                } else {
                  side.selected.remove(entry.name);
                }
              }),
            )
          : null,
    );
    return canSend
        ? Draggable<_Dragged>(
            data: selected
                ? _selection(side)
                : _Dragged(side.remote, List.of(side.path), [entry.name]),
            feedback: Material(
              elevation: 3,
              borderRadius: BorderRadius.circular(8),
              child: Padding(
                padding: const EdgeInsets.all(12),
                child: Text(
                  selected ? '${side.selected.length} 项' : entry.name,
                ),
              ),
            ),
            child: row,
          )
        : row;
  }

  Widget _taskRow(Map<String, Object?> task) {
    final phase = task['phase'] as String? ?? '';
    final id = task['id'] as int;
    final sent = task['bytes'] as int? ?? 0;
    final total = task['size'] as int? ?? 0;
    final title = switch (phase) {
      'queued' => '排队中',
      'scanning' => '扫描中',
      'transferring' => '${_size(sent)} / ${_size(total)}',
      'paused' => '已暂停',
      'reconnecting' => '等待重连',
      'complete' => '已完成',
      'canceled' => '已取消',
      _ => CollaborationIssue(task['error'] as String? ?? 'io').message,
    };
    return ListTile(
      dense: true,
      leading: Icon(
        task['direction'] == 'download' ? Icons.download : Icons.upload,
      ),
      title: Text('${task['label']} · $title', overflow: TextOverflow.ellipsis),
      subtitle: phase == 'transferring' && total > 0
          ? LinearProgressIndicator(value: (sent / total).clamp(0.0, 1.0))
          : null,
      trailing: _busy(task)
          ? Wrap(
              children: [
                IconButton(
                  tooltip: phase == 'paused' ? '继续' : '暂停',
                  icon: Icon(
                    phase == 'paused' ? Icons.play_arrow : Icons.pause,
                  ),
                  onPressed: () => _command({
                    'operation': 'task',
                    'task': id,
                    'control': phase == 'paused' ? 'resume' : 'pause',
                  }),
                ),
                IconButton(
                  tooltip: '取消任务',
                  icon: const Icon(Icons.cancel_outlined),
                  onPressed: () => _command({
                    'operation': 'task',
                    'task': id,
                    'control': 'cancel',
                  }),
                ),
              ],
            )
          : phase == 'failed' || phase == 'canceled'
          ? TextButton(
              onPressed: () => _command({'operation': 'retry', 'task': id}),
              child: const Text('从头重试'),
            )
          : null,
    );
  }
}

class _Browser {
  _Browser(this.remote);
  final bool remote;
  final entries = <_FileEntry>[];
  final selected = <String>{};
  List<String> path = const [];
  bool loading = false;
  bool bySize = false;
  String? cursor, issue;

  void sortEntries() {
    entries.sort((first, second) {
      if (first.folder != second.folder) return first.folder ? -1 : 1;
      final order = bySize && !first.folder
          ? second.size.compareTo(first.size)
          : 0;
      return order == 0 ? first.name.compareTo(second.name) : order;
    });
  }
}

class _FileEntry {
  const _FileEntry(this.name, this.folder, this.size);
  factory _FileEntry.fromJson(Map<String, Object?> json) => _FileEntry(
    json['name'] as String,
    json['kind'] == 'directory',
    json['size'] as int,
  );
  final String name;
  final bool folder;
  final int size;
}

class _Dragged {
  const _Dragged(this.remote, this.path, this.items);
  final bool remote;
  final List<String> path, items;
}

String _relative(List<String> value) =>
    value.isEmpty ? '/' : '/${value.join('/')}';
String _size(int value) => value < 1024
    ? '$value B'
    : (value < 1024 * 1024
          ? '${(value / 1024).toStringAsFixed(1)} KiB'
          : '${(value / (1024 * 1024)).toStringAsFixed(1)} MiB');

Map<String, Object?> _map(Object? value) =>
    value is Map ? value.cast<String, Object?>() : const {};
List<Map<String, Object?>> _maps(Object? value) => value is List
    ? value.whereType<Map>().map((v) => v.cast<String, Object?>()).toList()
    : const [];
