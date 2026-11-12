# Передача движка: что делает Клод, что не трогает

Этот документ — контракт. Клод занимается **движком и дизайном**, анонимность
остаётся за мной (инварианты ниже). Если правка требует нарушить инвариант —
это не правка движка, это вопрос ко мне.

Репозиторий: `sepia/`. Браузер: `sepia/browser/` (Rust, без сторонних browser-крейтов).

**Как передать:** дай Клоду доступ к этому файлу и к `sepia/browser/src/`, и попроси
работать по разделам 1–4. Первым сообщением достаточно:

> В репозитории есть `docs/HANDOFF-claude.md` — это твой контракт. Твоя зона:
> `browser/src/{html,dom,css,style,layout,paint,gui}.rs`. Задачи — раздел 3,
> порядок важен: шрифтовой фолбэк и эмодзи, картинки, таблицы, flex, типографика,
> потом дизайн окна. Инварианты из раздела 2 нарушать нельзя, API из раздела 1
> менять нельзя. Проверяй себя командами из раздела 4.

---

## 1. Границы: кто что делает

| Зона | Файлы | Кому |
|---|---|---|
| Вёрстка, CSS-каскад, парсер | `browser/src/{html,dom,css,style,layout}.rs` | **Клод** |
| Растеризация, шрифты, картинки | `browser/src/paint.rs`, `browser/src/fonts.rs` (можно добавить) | **Клод** |
| Окно и UI | `browser/src/gui.rs` | **Клод** (дизайн) |
| Загрузка страниц, диспетчер схем | `browser/src/page.rs`, `browser/src/main.rs` | совместно: API стабильно |
| Сеть, цепочка, DoH, оверлеи | `browser/src/{net,resolver,protocols}.rs`, `browser/src/url.rs` | **я** |
| CLI, файрвол, killswitch, верификация | `sepia/`, `core/` | **я** |

API, который нельзя менять без меня (на нём держится анонимность и CLI):

```rust
// page.rs — единственный вход «URL -> страница»
pub fn load(url: &Url, chain: &Chain, resolver: Option<&Resolver>,
            width: f32, fonts: &Fonts, timeout: Duration,
            insecure_tls: bool) -> Result<Loaded, String>;

// layout.rs
pub struct Fonts { ... }
impl Fonts { pub fn load() -> Fonts; pub fn pick(&self, bold: bool, italic: bool) -> &Font; }
impl Layout<'_> { pub fn new(fonts: &Fonts, sheet: &Stylesheet, width: f32) -> Layout;
                  pub fn build(self, root: &Node) -> Page; }

// paint.rs
impl Canvas { pub fn new(w: usize, h: usize, bg: Color) -> Canvas;
              pub fn paint(&mut self, fonts: &Fonts, page: &Page, scroll: f32);
              pub fn save_png(&self, path: &Path) -> io::Result<()>; }

// net.rs — сетевой слой (не переписывать)
pub fn fetch_with(url: &Url, chain: &Chain, resolver: Option<&Resolver>,
                  timeout: Duration) -> Result<Response, String>;
pub fn build_request(url: &Url) -> String;
pub fn request_headers(url: &Url) -> Vec<(String, String)>;
```

---

## 2. Инварианты анонимности (не нарушать ни при каких условиях)

1. **Никакого JavaScript.** Не добавлять JS-движок, не подключать v8/quickjs/boa.
2. **Никаких куки.** Не реализовывать Set-Cookie/Cookie их хранение или отправку.
3. **Никакого диска.** Ни кэша, ни профиля, ни истории, ни загрузок. Единственная
   запись — `--dump` PNG по явному пути.
4. **Только разрешённые заголовки.** Полный список — `net::request_headers`.
   Запрещено добавлять `Referer`, `Cookie`, `Authorization`, `Sec-CH-*`,
   `If-None-Match`, `DNT`, `X-Forwarded-*`, `Origin`.
5. **Только цепочка или DoH.** Любое соединение идёт через `net::fetch_with`
   / `protocols::*` / `resolver::Resolver`. Никаких `TcpStream::connect` в обход
   в других модулях, никакого `std::net::ToSocketAddrs` для адресов страниц.
6. **Никаких внешних запросов ради рендера**: шрифты только локальные
   (системные пути), без Google Fonts и CDN. Иконки/картинки — только из
   документа, через тот же `fetch_with`.
7. **Никакой телеметрии, обновлений, «проверки версии».**
8. **TLS не менять** (`rustls` + `webpki-roots`, SNI = хост, `--insecure` только
   для Gemini через явный флаг).
9. **Оверлеи не ломать**: `.onion` → Tor SOCKS, `.i2p` → I2P HTTP-прокси,
   `gemini://`, `gopher://` — свои клиенты в `protocols.rs`.
10. **`--audit` остаётся правдой.** Если появляется новая сетевая поверхность —
    она должна быть в аудите.

---

## 3. Задачи движку (по приоритету)

1. **Фолбэк шрифтов и эмодзи.** `Fonts` держит несколько фейсов; для codepoint,
   которого нет в Arial, брать следующий локальный фейс; эмодзи — Apple Color
   Emoji / Noto Color Emoji (билинейная интерполяция по глифу). Сейчас emoji
   рисуются квадратами.
2. **Картинки.** `<img src>`: PNG/JPEG-декодер, `width/height` атрибуты, `alt`,
   вписывание в контейнер, клик — ссылка. Загрузка — через `fetch_with`, не
   напрямую. `srcset`/`loading=lazy` можно игнорировать.
3. **Таблицы.** Настоящая модель `display: table/row/cell` с колонками,
   `border-collapse` хотя бы как 1px, выравнивание по ячейкам. Сейчас ячейки —
   инлайн-боксы (HN выглядит тесно).
4. **Flexbox-подмножество.** `display: flex`, `flex-direction`, `justify-content`,
   `align-items`, `gap`, `flex-grow`, `flex-wrap`. Это даст нормальный вид
   половине современных страниц (сейчас Vector-оболочка Wikipedia плывёт).
5. **Марджины и типографика.** Схлопывание вертикальных марджинов, `line-height`
   по каждому рану, `letter-spacing`, `text-indent`, базовый `max-width`.
6. **Производительность.** Wikipedia сейчас ~5000 текстовых узлов; замерить
   `layout + paint`, цель — < 150 мс на release-сборке, без аллокаций в цикле
   глифов.
7. **Дизайн окна (это к дизайнеру).** Адресная строка, кнопки назад/вперёд,
   индикатор транспорта (`tor`/`i2p`/`chain`/`direct`), экран ошибки с текстом
   причины, тёмная тема. Всё — в `gui.rs` через `Canvas`, цвета в одном месте.
8. **Красивый экран «нет JavaScript»**: аккуратная плашка вверху страницы вместо
   молчаливого пустого места — но без сетевых запросов.

---

## 4. Как проверять (Definition of Done)

```bash
cd browser
cargo build --release
cargo test --release          # сейчас 48 тестов, должно остаться зелёным и расти

# рендер-проверки (детерминированные, без сети):
./target/release/sepia-browser --url /путь/к/тесту.html --dump /tmp/a.png --width 900 --height 700
# сравнить глазами; скриншоты сравнивать глазами и НЕ коммитить

# живые страницы:
./target/release/sepia-browser --url https://news.ycombinator.com --dump /tmp/hn.png
./target/release/sepia-browser --url https://en.wikipedia.org/wiki/Web_browser --dump /tmp/wiki.png

# анонимность не сломана (мой чек-лист, должен проходить без правок):
./target/release/sepia-browser --audit
./target/release/sepia-browser --url gemini://mozz.us/ --text
./target/release/sepia-browser --via tor https://example.com --text   # ошибка про tor, не про DNS
./target/release/sepia-browser --url http://x.onion/ --text           # подсказка про --via tor
```

Приёмка задачи движка:

* `cargo test --release` зелёный, новые функции покрыты тестами;
* `--audit` печатает ту же поверхность (никаких новых заголовков/сетей);
* рендер трёх страниц (`example.com`, HN, Wikipedia) не хуже текущего (сравнение глазами);
* в диффе нет `TcpStream::connect`, `ToSocketAddrs`, `Cookie`, `Referer`,
  `std::fs::write` вне `--dump`.

---

## 5. Что я делаю параллельно (анонимность)

* регрессионные тесты на поверхность: заголовки, отсутствие куки, отсутствие
  прямых сокетов (`net::request_headers` уже под тестом);
* ротация цепочки между запросами (разные exit'ы на разные сайты);
* проверка «нет DNS-утечки» как автотест: DoH обязан быть вызван, системный
  резолвер — нет;
* `sepia verify` и `sepia selftest` для нативного браузера (exit IP, отсутствие
  WebRTC, консистентность отпечатка);
* живой прогон Tor/I2P, когда появятся демоны, и живые тесты Gemini/Gopher;
* жёсткий лимит: браузер не должен уметь писать на диск ничего, кроме `--dump`.
