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
- **exit-node режим** — запуск этой машины крайней точкой туннеля для других пиров.

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
make test       # cargo test
```

Установка движка (выдаёт `cap_net_admin`, чтобы работал fwmark в TUN; TUN требует root):

```sh
make install    # pkexec
make install-gui
```

Релизные пакеты:

```sh
make dist GUI=1            # Linux: tar.gz + zip в dist/
make dist-windows GUI=1    # Windows: сборка + wintun.dll (нужны mingw-w64 и rustup target)
```

## Использование

```sh
openflux import  <ссылка-на-документ>      # импорт профиля из контроль-плоскости
openflux add-profile demo --doc-url <url> \
    --streams 2 --dns tls://1.1.1.1 \
    --split-mode exclude --split-domains '*.ya.ru'
openflux edit-profile demo --streams 3
openflux connect                           # SOCKS5-режим клиента
openflux proxy on                          # системный прокси -> SOCKS5
openflux tun on                            # TUN-режим (pkexec; Windows: повышение прав)
openflux exit on|off                       # запуск этой машины exit-node'ом
openflux status | logs | tui
```

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