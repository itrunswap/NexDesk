# 自托管部署

一套服务提供设备目录、WebRTC 信令、浏览器页面和临时 TURN 凭据。桌面、浏览器、移动端和 CLI 使用同一个 HTTPS 地址。设备之间优先建立 WebRTC 直连，需要时由 TURN 转发。局域网的原生 IP 直连独立于这套服务。

## 先在本机运行

从 [发行页](https://github.com/itrunswap/NexDesk/releases/latest) 下载与服务器操作系统对应的 `SelfHost` 包。Windows 解压后进入该目录，在 PowerShell 执行 `.\nexdesk-signaling.exe`。Linux 解压后执行以下命令。

```sh
export NEXDESK_WEB_ROOT="$PWD/web"
./bin/nexdesk-signaling
```

打开 `http://127.0.0.1:54000` 查看页面，访问 `/healthz` 应返回 `{"ok":true,"protocol":1}`。默认监听回环地址，设备数据库创建在当前工作目录。使用 `NEXDESK_SIGNAL_DB` 可指定持久化数据库路径。开发时把本机桌面端的自托管地址填为 `http://127.0.0.1:54000`。跨设备使用下列 HTTPS 部署。

## Linux Docker Compose

在 Linux 服务器安装 Docker Engine 和 Docker Compose 插件。准备两端都能解析的域名，将 DNS 指向服务器，并开放公网 HTTPS 所用的 TCP 443。Clone 源码后，在仓库根目录执行以下命令。

```sh
cp deploy/.env.example deploy/.env
chmod 600 deploy/.env
```

编辑 `deploy/.env`，把域名替换成自己的。

```dotenv
NEXDESK_HOST=desk.example.com
NEXDESK_HTTPS_PORT=443
NEXDESK_CADDYFILE=./Caddyfile.public
```

```sh
docker compose --env-file deploy/.env -f deploy/compose.yaml up -d --build
docker compose --env-file deploy/.env -f deploy/compose.yaml ps
curl -fsS https://desk.example.com/healthz
```

模板把目录数据保存在 `directory-data` 卷，目录监听地址仅在容器网络内。Caddy 的证书放在 `caddy-data` 卷。内网 CA 使用默认 `Caddyfile`，取出 `/data/caddy/pki/authorities/local/root.crt` 并在各客户端信任。外部 HTTPS 端口改为 9443 等非标准端口时，设置 `NEXDESK_HTTPS_PORT` 和包含端口的 `NEXDESK_SIGNAL_ORIGIN`，并配置可用的证书。[Caddy 自动 HTTPS](https://caddyserver.com/docs/automatic-https)

## Linux Nginx 与 systemd

适用于已有 Nginx 的服务器。下载并校验对应版本的 `NexDesk-<版本号>-SelfHost-Linux-x64.tar.gz`，放到当前目录。以下命令适用于 Debian/Ubuntu 上的首次部署。已有同名服务、目录或 Nginx 站点时，在复制前核对现有配置。

```sh
version=0.1.4
sudo apt-get install nginx certbot
getent passwd nexdesk >/dev/null || sudo useradd --system --no-create-home --user-group --shell /usr/sbin/nologin nexdesk
sudo install -d -m 755 "/opt/nexdesk/releases/$version" /var/lib/nexdesk-acme
sudo install -d -m 750 /etc/nexdesk
sudo tar -xzf "NexDesk-$version-SelfHost-Linux-x64.tar.gz" -C "/opt/nexdesk/releases/$version"
sudo ln -s "/opt/nexdesk/releases/$version" /opt/nexdesk/current
sudo install -m 644 /opt/nexdesk/current/nginx/nexdesk-signaling.service /etc/systemd/system/
sudo install -m 600 /dev/null /etc/nexdesk/signaling.env
sudoedit /etc/nexdesk/signaling.env
```

填入以下环境变量，用自己的 HTTPS 域名。继续使用现有注册库时，请保留原数据库路径。

```dotenv
NEXDESK_SIGNAL_LISTEN=127.0.0.1:54000
NEXDESK_SIGNAL_ORIGIN=https://desk.example.com
NEXDESK_SIGNAL_DB=/var/lib/nexdesk/directory.sqlite3
NEXDESK_WEB_ROOT=/opt/nexdesk/current/web
NEXDESK_TRUSTED_PROXY=127.0.0.1
NEXDESK_METRICS_LISTEN=127.0.0.1:54001
```

```sh
sudo systemctl daemon-reload
sudo systemctl enable --now nexdesk-signaling
curl -fsS http://127.0.0.1:54000/healthz
```

如果域名已有有效证书，使用包内的 `nginx/nexdesk.conf.example`，按实际情况替换域名与证书路径。首次使用 Certbot 可先仅启用模板的 80 端口 `server`，保留 `/.well-known/acme-challenge/` 的 webroot。开放 TCP 80，执行 `sudo nginx -t && sudo systemctl reload nginx`，再申请证书。

```sh
sudo certbot certonly --webroot -w /var/lib/nexdesk-acme -d desk.example.com
```

取得证书后，将完整模板放到 `/etc/nginx/conf.d/nexdesk.conf` 并按上面的域名、证书和代理地址调整。避免让同一域名的临时站点与正式站点同时加载。Nginx 需要转发 WebSocket 的 `Upgrade` 和 `Connection`，并覆盖 `X-NexDesk-Source`。加载站点后检查 HTTPS 页面和 API。使用 webroot 续期时应配置成功续期后的 Nginx reload。

```sh
sudo nginx -t && sudo systemctl reload nginx
curl -fsS https://desk.example.com/healthz
sudo journalctl -u nexdesk-signaling -n 50 --no-pager
```

`54000` 和 `54001` 保持回环监听。公网访问经 HTTPS 入口。[Nginx WebSocket 代理](https://nginx.org/en/docs/http/websocket.html) · [Certbot webroot](https://eff-certbot.readthedocs.io/en/stable/using.html#webroot)

## TURN 与跨网连接

被控端和主控端没有可用的 ICE 直连路径时，需要一个两端都能访问的 TURN 服务。Compose 可以使用附带的 coturn。在 `deploy/.env` 或目录进程环境里配置下面两个变量。

```dotenv
NEXDESK_TURN_URLS="turn:desk.example.com:3478?transport=udp turn:desk.example.com:3478?transport=tcp"
NEXDESK_TURN_SECRET=填写与coturn相同的至少32字符随机值
```

使用 `openssl rand -hex 32` 生成专用随机值。云防火墙和系统防火墙开放 3478 TCP/UDP，以及 49160–49259 UDP。如果服务器在静态 NAT 后，按本机地址和公网地址设置 `NEXDESK_TURN_BIND_IP` 与 `NEXDESK_TURN_EXTERNAL_IP`，后者接受 `公网IP/内网IP`。独立 coturn 使用同一共享密钥和附带的 `turn/turnserver.conf` 作为基线。

```sh
docker compose --env-file deploy/.env -f deploy/compose.yaml --profile turn up -d --build
```

验收时可以暂设 `NEXDESK_ICE_RELAY_ONLY=1`，重启目录后从不同网段建立会话并查看连接方式。完成后恢复默认 `0`，由 ICE 优先选用可达路径。

## 客户端接入与维护

1. 被控电脑在「设置 → 连接」启用自托管，填写域名和 HTTPS 端口。主控桌面使用同一地址，浏览器或移动端打开同一 HTTPS 服务。
2. 确认设备 ID 在线，输入该 ID 和被控端显示的密码完成连接。单独测试局域网 IP 直连时使用被控端 IP、监听端口和 IP 模式密码。
3. 需要限制新设备注册时配置 `NEXDESK_ENROLLMENT_KEY`，并在首次注册的被控应用进程环境中提供同一值。
4. 运行 `nexdesk-signaling backup DATABASE NEW_FILE` 生成在线 SQLite 快照。保存部署配置和证书，升级前停掉目录进程，切换到新版本目录后检查 `/healthz`。备份文件和配置中的密钥应存放在受控位置。
