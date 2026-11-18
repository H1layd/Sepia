# Sepia

Браузер со своим движком, написанный с нуля на Rust. Внутри нет ни Chromium,
ни WebKit, ни WebView. HTTP, HTML, CSS, раскладка, отрисовка, шрифты и окно —
свои. JavaScript исполняется в [Boa](https://github.com/boa-dev/boa) поверх
собственного DOM.

Цель — обычный браузер, в котором сайты открываются и выглядят так же, как
в Chrome или Firefox. Анонимность появится позже отдельным слоем поверх
готового браузера (см. [ниже](#анонимность-и-cli)).

> **Статус: ранняя стадия.** Текстовые сайты, новости, документация и форумы
> открываются хорошо. Тяжёлые приложения (GitHub, YouTube, Reddit, Figma)
> открываются, но частично и медленно. Честный список того, что работает и
> чего нет, — в [`docs/COMPATIBILITY.md`](docs/COMPATIBILITY.md).

## Запуск

Нужен Rust (stable).

```bash
cd browser
cargo run --release -- --gui                       # окно браузера
cargo run --release -- --url https://example.com   # окно сразу с этой страницей
```

Собранный бинарник — `browser/target/release/sepia-browser`.

Без окна, для проверки и отладки:

```bash
sepia-browser --url https://en.wikipedia.org/wiki/Web_browser --dump page.png   # снимок страницы
sepia-browser --url https://news.ycombinator.com/ --text                         # разложенный текст и сводка
sepia-browser --url https://www.youtube.com/ --text --wait 8000                 # дать скриптам поработать 8 с
sepia-browser --audit                                                            # что браузер отправляет и хранит
```

## Что внутри

| Часть | Что делает |
|---|---|
| **Сеть** | HTTP/1.1 и TLS (rustls), keep-alive, редиректы, gzip/brotli. Системный прокси, как у обычного браузера (переменные окружения, иначе настройки macOS). CORS, Referer и Origin по правилам браузеров. Cookies в памяти, раздельно по сайтам. Кодировки: UTF-8, windows-1251, KOI8-R и другие |
| **HTML и DOM** | Свой парсер; DOM, доступный скриптам; Shadow DOM, `<template>`, кастом-элементы |
| **CSS** | Каскад, специфичность, наследование, переменные `var()`, `calc()`, media-запросы, `@supports`, `@font-face`. Flex, grid (частично), таблицы, float, позиционирование. Тёмная тема по системной: `prefers-color-scheme`, `color-scheme`, `light-dark()` |
| **Раскладка и отрисовка** | Блочный и строчный поток, перенос по метрикам глифов, в том числе для CJK. Фоны, рамки, скругления, тени, градиенты |
| **JavaScript** | Boa с собственными исправлениями (`browser/vendor/`). ES-модули и import maps, `fetch` и XMLHttpRequest, таймеры и промисы, `MutationObserver`, `ResizeObserver`, `IntersectionObserver`, `matchMedia`, IndexedDB и CacheStorage в памяти, `Intl`. Скрипты видят настоящую геометрию (`getBoundingClientRect`, `offsetWidth`, `getComputedStyle`) |
| **Текст и картинки** | Шрифты TTF/OTF/TTC/WOFF/WOFF2 с фолбэком по символам; качаются только нужные наборы символов (`unicode-range`). Цветные эмодзи. Картинки PNG, JPEG, GIF, WebP, SVG, ICO |
| **Окно** | Вкладки, адресная строка с поиском, история, закладки, поиск по странице, скачивание. Настройки: страна и то, каким браузером представляться (язык, часовой пояс, `User-Agent`) |
| **Протоколы** | Кроме http(s): Gemini, Gopher, адреса `.onion` и `.i2p` через Tor и I2P |

Клавиши: `⌘L` — адрес, `Enter` — открыть или найти, `⌘T`/`⌘W` — вкладка,
`⌘R` — обновить, `⌘F`/`⌘G` — поиск по странице, `⌘D` — в закладки,
`⌘S` — сохранить страницу, `⌥`-клик — скачать ссылку, свайпы — назад и вперёд.

## Чего пока нет

Подробно — в [`docs/COMPATIBILITY.md`](docs/COMPATIBILITY.md). Главное:

- **Скорость JS.** Boa во много раз медленнее V8: тяжёлые сайты грузятся
  десятки секунд.
- **CSS:** grid целиком, `position: sticky`, анимации и переходы,
  `:hover`/`:active`, `opacity` у текста. Нет настоящих полей ввода.
- **Платформа:** нет Web Workers, WebSocket из скриптов, Service Worker,
  WebGL, воспроизведения видео и звука. `<canvas>` рисует только в 2D.
- **Блокировки:** сайты, которые закрываются от автоматики (403, CAPTCHA),
  пока показывают пустую страницу вместо объяснения.
- **Защита от зависших скриптов:** ограничений по времени и размеру сейчас нет
  совсем, страница с бесконечным циклом вешает свою вкладку.

## Проверка

```bash
cd browser && cargo test --release          # 214 тестов
python3 scripts/qa.py                       # сценарии по сайтам: текст, пиксели, оценка
python3 scripts/visual-diff.py --url https://en.wikipedia.org/wiki/Web_browser
                                            # сравнение с Chrome: сходство и композит
```

Для отладки движка есть переменные окружения:

| Переменная | Что делает |
|---|---|
| `SEPIA_VERBOSE=1` | время по фазам загрузки, медленные скрипты, консоль страницы |
| `SEPIA_EVAL='код'` | выполнить JS после загрузки и напечатать результат |
| `SEPIA_THROWS=1` | печатать каждое исключение JS со стеком |
| `SEPIA_DUMP=папка` | сохранить все скрипты страницы |
| `SEPIA_CSS_DUMP=файл` | сохранить CSS, который добавили скрипты |
| `SEPIA_PRELUDE_EXTRA='код'` | выполнить JS до скриптов страницы |
| `SEPIA_COLOR_SCHEME=light\|dark` | тема вместо системной |
| `SEPIA_NO_SYSTEM_PROXY=1` | не использовать системный прокси |

## Анонимность и CLI

Сейчас браузер ведёт себя как обычный: отправляет Referer, грузит все
картинки, представляется распространённым браузером. Слой анонимности —
политики хранилища, отпечаток, цепочки прокси, DoH, killswitch — вводится
поверх браузера отдельно, когда сайты будут открываться как надо.
Документы: [`docs/PRIVACY-LAYER.md`](docs/PRIVACY-LAYER.md) — дизайн слоя,
[`docs/THREAT-MODEL.md`](docs/THREAT-MODEL.md) — от кого он защищает и от кого
нет, [`docs/HANDOFF-claude.md`](docs/HANDOFF-claude.md) — кто что делает и
точки стыковки.

В репозитории есть и инструменты этого слоя. Сейчас они на втором плане:

- `sepia/` — Python CLI: цепочка прокси, маршруты (`direct`, `chain`, `tor`,
  `i2p`…), killswitch, системный прокси, проверка утечек. Запуск —
  `./sepia.sh --help`.
- `core/` — `sepia-core` на Rust: SOCKS5/HTTP-прокси через цепочку и
  DoH-резолвер.
- `native/` — `sepiaprobe` на C++: подсчёт отпечатка TLS (JA3).

Собрать всё и прогнать тесты: `bash scripts/build.sh`.

## Структура

```
browser/            браузер (Rust)
  src/              net, html, dom, css, style, layout, paint, fonts, images,
                    js + js_prelude.js, page, gui, persona, charset, protocols
  vendor/           Boa с исправлениями Sepia
docs/               совместимость, спецификация, QA, приватность, модель угроз
scripts/            qa.py, visual-diff.py, build.sh
sepia/ core/ native/    слой анонимности: CLI, прокси-ядро, TLS-пробник
tests/              тесты CLI и e2e-сценарии
```

## Лицензия

MIT, см. [`LICENSE`](LICENSE).
