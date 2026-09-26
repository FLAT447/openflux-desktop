# OpenFlux desktop client

[English](README.md) | **Русский**

Десктопный клиент для контроль-плоскости в стиле [Яндекс Документов](https://yandex.ru/dev/docs/)
(туннельные конфиги, раздаваемые «как документ»). Один менеджер-процесс (`openflux`)
и Go-движок (`openflux-engine`) дают:

- **SOCKS5**-режим клиента (`connect` / кнопка Connect в GUI) — классическая точка входа;
- **системный прокси** — наведение настроек прокси GNOME/KDE на локальный SOCKS5;
- **TUN-режим** — захват всего трафика системы через маршрутизацию (fwmark держит сокеты
  самого движка вне его туннеля на Linux, отдельные маршруты `/1` на Windows);
- **мультистрим** — 1–8 параллельных WebSocket-стримов на сессию;
- **split-туннелирование** — `exclude` (указанные сайты идут мимо туннеля) или `include`
  (через туннель идут только указанные);
- **шифрованный DNS** для TUN-шлюза — обычный `ip[:port]`, `tls://host` (DoT) или
  `https://host/path` (DoH);
- **exit-node режим** — запуск этой машины крайней точкой туннеля для других пиров;
- **выбор транспорта** — `yandex`, `volga`, `oneme`, `yandex_multistream`,
  `cupsonline`, `mailru` и экспериментальный `boards`, с кодеками `legacy`/`batched`
  для универсальных транспортов.

Три интерфейса делят одну реализацию: CLI, TUI (`openflux tui`) и [Tauri](https://tauri.app)
GUI (`openflux-gui`) вызывают одну и ту же библиотеку `openflux::actions`, поэтому они
никогда не расходятся между собой.

## Структура

```
src/        Rust-библиотека + CLI/TUI, платформенные TUN-бэкенды (src/tun/)
gui/        Десктопный GUI на Tauri v2 (статичный фронтенд, вшивается при сборке)
engine/     Go-движок (WebSocket-туннель, стримы, split, DoT/DoH, exit-node)
packaging/  Скрипты установки/удаления и .desktop-файл
```

## Ядро

Этот клиент работает с сервером OpenFlux и туннелируется через него:

[**wlruscfd/openflux-server**](https://github.com/wlruscfd/openflux-server) — контроль-плоскость
и туннельная инфраструктура, к которой подключается клиент.

## Сборка

Linux (нужны toolchain'и Rust/Go, а для GUI также `webkit2gtk-4.1`, `librsvg`, `dbus`
и заголовки X):

```sh
make            # cargo build --release + go build, копирует лаунчер в bin/
make gui        # собирает gui/target/release/openflux-gui
make lint       # cargo clippy -D warnings (оба крейта) + go vet
make test       # cargo test (CLI + GUI) + go test (движок)
```

Установка движка (выдаёт `cap_net_admin`, чтобы работал fwmark в TUN; TUN требует root):

```sh
make install    # pkexec
make install-gui
```

Релизные пакеты:

```sh
make dist GUI=1            # Linux: tar.gz в dist/ (имя вида openflux-<ver>-linux-<arch>)
make dist-windows GUI=1    # Windows: zip (openflux-<ver>-windows-<arch>.zip) + wintun.dll (нужны mingw-w64 и rustup target)
```

## Использование

```sh
openflux import  <ссылка-на-документ>      # импорт профиля из контроль-плоскости
openflux add-profile demo --transport yandex --doc-url <url> --streams 2
openflux add-profile multi --transport yandex_multistream \
    --doc-urls <url-1>,<url-2> --streams 2
openflux add-profile max --transport oneme \
    --max-token <token> --max-uid <uid> --streams 1
openflux add-profile flaky --transport yandex --doc-url <url> \
    --captcha-solve-mode headless_browser
openflux edit-profile demo --streams 3
openflux settings show                     # SOCKS5-порт, DNS, раздельный туннель
openflux settings set --socks-port 1080 --dns tls://1.1.1.1 \
    --split-mode exclude --split-domains '*.ya.ru'
openflux connect                           # SOCKS5-режим клиента
openflux proxy on                          # системный прокси -> SOCKS5
openflux tun on                            # TUN-режим (pkexec; Windows: повышение прав)
openflux exit on|off                       # запуск этой машины exit-node'ом
openflux status | logs | tui
```

SOCKS5-порт, DNS для TUN и раздельный туннель — общие настройки машины
(`openflux settings`, в GUI кнопка «⚙» или «Настройки»), а не настройки профиля:
они действуют для всех профилей сразу. Streams, MTU и captcha остаются per-profile.

Для `yandex_multistream` нужны минимум два URL документа. Для `oneme` нужны
положительное числовое значение `--max-uid` и `--max-token`. Сейчас контроль-плоскость
предоставляет управляемые транспорты `yandex`, `yandex_multistream`, `mailru` и `boards`;
остальные можно настроить вручную через CLI или GUI.

`--captcha-solve-mode headless_browser` (только для транспортов Яндекса) заставляет
движок открыть документ в локальном headless-браузере и сам пройти проверку; нужен
установленный Chrome/Chromium. По умолчанию `off` — о блокировке только сообщается в
журнале движка.

В TUI: `c` connect, `d` disconnect, `p` proxy, `t` TUN (приостанавливается ради polkit),
`e` exit node, `Enter` активировать профиль, `q` выход.

GUI предоставляет те же действия; кнопка TUN на Windows перезапускает бинарник с
повышением прав (`ShellExecuteW runas openflux-gui tun on|off`, через UAC).

## Конфигурация и состояние

- Конфиг: `~/.config/openflux/openflux.toml` (переопределяется через `OPENFLUX_CONFIG_DIR`).
- Рядом пишутся состояние движка (pid-файлы, лог, снимок настроек прокси).
- Профили правятся в CLI или GUI; ключ — это shared-link токен контроль-плоскости.
  Никогда не коммитьте реальные ключи и токены.

## Платформы

| Возможность  | Linux                      | Windows                        |
|--------------|----------------------------|--------------------------------|
| TUN          | fd `/dev/net/tun` + fwmark | адаптер Wintun + netsh `/1`    |
| Привилегии   | `pkexec`                   | UAC (`runas`)                  |
| Exit node    | `raw` (нужен root)         | `proxy` (только TCP, без root) |

Для Windows TUN требуется Wintun 0.14 (`wintun.dll`) рядом с исполняемым файлом;
Windows-путь проверен только компиляцией и не обкатан на реальном Windows-хосте.

## Лицензия

GPL-3.0-or-later — см. [LICENSE](LICENSE).