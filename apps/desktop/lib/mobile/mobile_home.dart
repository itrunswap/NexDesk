import 'dart:async';
import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:flutter/gestures.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../connection/directory_origin.dart';
import '../ui/nexdesk_theme.dart';

class MobileHome extends StatefulWidget {
  const MobileHome({super.key});

  @override
  State<MobileHome> createState() => _MobileHomeState();
}

class _MobileHomeState extends State<MobileHome> {
  static const _profile = MethodChannel('dev.nexdesk/mobile_profile');
  String? _origin, _issue;
  bool _loading = true;
  int _reload = 0;

  @override
  void initState() {
    super.initState();
    unawaited(_load());
  }

  Future<void> _load() async {
    try {
      final saved = await _profile.invokeMethod<String>('getDirectory');
      if (mounted) {
        setState(() {
          _origin = _httpsOrigin(saved ?? defaultDirectoryOrigin);
          _loading = false;
        });
      }
    } on PlatformException {
      if (mounted) {
        setState(() {
          _issue = '无法读取连接服务设置。';
          _loading = false;
        });
      }
    }
  }

  static String? _httpsOrigin(String value) {
    final origin = directoryOriginFromFields(value, '');
    return origin != null && Uri.parse(origin).scheme == 'https'
        ? origin
        : null;
  }

  Future<void> _configure() async {
    final origin = await showDialog<String>(
      context: context,
      builder: (_) => _ServiceDialog(origin: _origin, parse: _httpsOrigin),
    );
    if (!mounted || origin == null || origin == _origin) return;
    try {
      await _profile.invokeMethod<void>('saveDirectory', origin);
      if (mounted) {
        setState(() {
          _origin = origin;
          _issue = null;
        });
      }
    } on PlatformException {
      if (mounted) setState(() => _issue = '无法保存连接服务设置。');
    }
  }

  @override
  Widget build(BuildContext context) {
    final origin = _origin;
    return Scaffold(
      appBar: AppBar(
        title: Row(
          children: [
            const NexDeskMark(size: 30),
            const SizedBox(width: 9),
            Text('NexDesk', style: Theme.of(context).textTheme.titleLarge),
          ],
        ),
        shape: Border(
          bottom: BorderSide(
            color: Theme.of(context).colorScheme.outlineVariant,
          ),
        ),
        actions: [
          if (origin != null)
            IconButton(
              onPressed: () => setState(() => _reload++),
              tooltip: '重新加载连接页面',
              icon: const Icon(Icons.refresh),
            ),
          IconButton(
            onPressed: _loading ? null : _configure,
            tooltip: '连接服务设置',
            icon: const Icon(Icons.settings_outlined),
          ),
        ],
      ),
      body: SafeArea(
        child: Column(
          children: [
            if (_issue case final issue?)
              MaterialBanner(
                content: Text(issue),
                actions: [
                  TextButton(onPressed: _configure, child: const Text('设置')),
                ],
              ),
            Expanded(
              child: _loading
                  ? const Center(child: CircularProgressIndicator())
                  : origin == null
                  ? Center(
                      child: FilledButton.icon(
                        onPressed: _configure,
                        icon: const Icon(Icons.dns_outlined),
                        label: const Text('配置连接服务'),
                      ),
                    )
                  : _MobileRemoteView(
                      key: ValueKey((origin, _reload)),
                      origin: origin,
                    ),
            ),
          ],
        ),
      ),
    );
  }
}

class _ServiceDialog extends StatefulWidget {
  const _ServiceDialog({required this.origin, required this.parse});

  final String? origin;
  final String? Function(String) parse;

  @override
  State<_ServiceDialog> createState() => _ServiceDialogState();
}

class _ServiceDialogState extends State<_ServiceDialog> {
  late final _address = TextEditingController(text: widget.origin);
  String? _issue;

  void _save() {
    final origin = widget.parse(_address.text);
    if (origin != null) {
      Navigator.pop(context, origin);
    } else {
      setState(() => _issue = '请输入目录服务的 HTTPS 根地址。');
    }
  }

  @override
  void dispose() {
    _address.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => AlertDialog(
    title: const Text('连接服务'),
    content: TextField(
      controller: _address,
      keyboardType: TextInputType.url,
      autocorrect: false,
      enableSuggestions: false,
      onSubmitted: (_) => _save(),
      decoration: InputDecoration(
        labelText: '服务器地址和端口',
        hintText: 'https://desk.example.test',
        errorText: _issue,
      ),
    ),
    actions: [
      TextButton(
        onPressed: () => Navigator.pop(context),
        child: const Text('取消'),
      ),
      FilledButton(onPressed: _save, child: const Text('连接此服务')),
    ],
  );
}

class _MobileRemoteView extends StatefulWidget {
  const _MobileRemoteView({super.key, required this.origin});

  final String origin;

  @override
  State<_MobileRemoteView> createState() => _MobileRemoteViewState();
}

class _MobileRemoteViewState extends State<_MobileRemoteView>
    with WidgetsBindingObserver {
  static const _viewType = 'dev.nexdesk/mobile_web';
  MethodChannel? _events;

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
  }

  void _ready(int viewId) {
    _events = MethodChannel('dev.nexdesk/mobile_web/$viewId');
    didChangeAppLifecycleState(
      WidgetsBinding.instance.lifecycleState ?? AppLifecycleState.resumed,
    );
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    unawaited(
      _events
              ?.invokeMethod<void>(
                'foreground',
                state == AppLifecycleState.resumed,
              )
              .catchError((Object _) {}) ??
          Future<void>.value(),
    );
  }

  @override
  void dispose() {
    didChangeAppLifecycleState(AppLifecycleState.detached);
    WidgetsBinding.instance.removeObserver(this);
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final settings = {'origin': widget.origin};
    final gestures = {
      Factory<OneSequenceGestureRecognizer>(EagerGestureRecognizer.new),
    };
    if (Platform.isAndroid) {
      return AndroidView(
        viewType: _viewType,
        creationParams: settings,
        creationParamsCodec: const StandardMessageCodec(),
        gestureRecognizers: gestures,
        onPlatformViewCreated: _ready,
      );
    }
    return UiKitView(
      viewType: _viewType,
      creationParams: settings,
      creationParamsCodec: const StandardMessageCodec(),
      gestureRecognizers: gestures,
      onPlatformViewCreated: _ready,
    );
  }
}
