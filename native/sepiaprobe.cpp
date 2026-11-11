// sepiaprobe — what does our TLS handshake actually look like?
//
// Crafted ClientHello, hashed locally (JA3), optionally pushed through a SOCKS5
// hop so you can see the handshake the far side receives. No external libraries:
// MD5 is implemented here, the socket layer is plain POSIX.
//
// Build:  c++ -std=c++17 -O2 -o sepiaprobe sepiaprobe.cpp
// Usage:  ./sepiaprobe --sni example.com [--through 127.0.0.1:10800] [--json]

#include <arpa/inet.h>
#include <fcntl.h>
#include <netdb.h>
#include <netinet/in.h>
#include <sys/socket.h>
#include <sys/types.h>
#include <unistd.h>

#include <algorithm>
#include <cstdint>
#include <ctime>
#include <cstdlib>
#include <cstdio>
#include <cstring>
#include <string>
#include <vector>

namespace {

// --------------------------------------------------------------------------
// MD5 (RFC 1321) — only what JA3 needs
// --------------------------------------------------------------------------

struct Md5 {
  uint32_t a = 0x67452301, b = 0xefcdab89, c = 0x98badcfe, d = 0x10325476;

  static uint32_t rotl(uint32_t x, int n) { return (x << n) | (x >> (32 - n)); }

  void block(const uint8_t* p) {
    static const uint32_t K[64] = {
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a,
        0xa8304613, 0xfd469501, 0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be,
        0x6b901122, 0xfd987193, 0xa679438e, 0x49b40821, 0xf61e2562, 0xc040b340,
        0x265e5a51, 0xe9b6c7aa, 0xd62f105d, 0x02441453, 0xd8a1e681, 0xe7d3fbc8,
        0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed, 0xa9e3e905, 0xfcefa3f8,
        0x676f02d9, 0x8d2a4c8a, 0xfffa3942, 0x8771f681, 0x6d9d6122, 0xfde5380c,
        0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70, 0x289b7ec6, 0xeaa127fa,
        0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665,
        0xf4292244, 0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92,
        0xffeff47d, 0x85845dd1, 0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1,
        0xf7537e82, 0xbd3af235, 0x2ad7d2bb, 0xeb86d391};
    static const int S[64] = {7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7,
                              12, 17, 22, 5, 9,  14, 20, 5, 9,  14, 20, 5, 9,
                              14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16,
                              23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15, 21,
                              6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21};
    uint32_t M[16];
    for (int i = 0; i < 16; ++i) {
      M[i] = static_cast<uint32_t>(p[i * 4]) |
             (static_cast<uint32_t>(p[i * 4 + 1]) << 8) |
             (static_cast<uint32_t>(p[i * 4 + 2]) << 16) |
             (static_cast<uint32_t>(p[i * 4 + 3]) << 24);
    }
    uint32_t A = a, B = b, C = c, D = d;
    for (int i = 0; i < 64; ++i) {
      uint32_t F;
      int g;
      if (i < 16) {
        F = (B & C) | (~B & D);
        g = i;
      } else if (i < 32) {
        F = (D & B) | (~D & C);
        g = (5 * i + 1) % 16;
      } else if (i < 48) {
        F = B ^ C ^ D;
        g = (3 * i + 5) % 16;
      } else {
        F = C ^ (B | ~D);
        g = (7 * i) % 16;
      }
      uint32_t tmp = D;
      D = C;
      C = B;
      B = B + rotl(A + F + K[i] + M[g], S[i]);
      A = tmp;
    }
    a += A;
    b += B;
    c += C;
    d += D;
  }

  std::string hex(const std::string& message) {
    std::vector<uint8_t> data(message.begin(), message.end());
    uint64_t bits = static_cast<uint64_t>(data.size()) * 8;
    data.push_back(0x80);
    while (data.size() % 64 != 56) data.push_back(0);
    for (int i = 0; i < 8; ++i) data.push_back(static_cast<uint8_t>(bits >> (8 * i)));
    for (size_t i = 0; i < data.size(); i += 64) block(&data[i]);
    uint32_t words[4] = {a, b, c, d};
    char out[33];
    for (int i = 0; i < 4; ++i) {
      std::snprintf(out + i * 8, 9, "%02x%02x%02x%02x", words[i] & 0xff,
                    (words[i] >> 8) & 0xff, (words[i] >> 16) & 0xff,
                    (words[i] >> 24) & 0xff);
    }
    return std::string(out, 32);
  }
};

// --------------------------------------------------------------------------
// byte helpers
// --------------------------------------------------------------------------

struct Buf {
  std::vector<uint8_t> bytes;
  void u8(uint8_t v) { bytes.push_back(v); }
  void u16(uint16_t v) {
    bytes.push_back(static_cast<uint8_t>(v >> 8));
    bytes.push_back(static_cast<uint8_t>(v & 0xff));
  }
  void u24(uint32_t v) {
    bytes.push_back(static_cast<uint8_t>(v >> 16));
    bytes.push_back(static_cast<uint8_t>((v >> 8) & 0xff));
    bytes.push_back(static_cast<uint8_t>(v & 0xff));
  }
  void raw(const void* p, size_t n) {
    const auto* b = static_cast<const uint8_t*>(p);
    bytes.insert(bytes.end(), b, b + n);
  }
  size_t size() const { return bytes.size(); }
  void patch16(size_t at, uint16_t v) {
    bytes[at] = static_cast<uint8_t>(v >> 8);
    bytes[at + 1] = static_cast<uint8_t>(v & 0xff);
  }
};

std::string rand_bytes(size_t n) {
  std::string out(n, '\0');
  int fd = ::open("/dev/urandom", O_RDONLY);
  if (fd >= 0) {
    size_t got = 0;
    while (got < n) {
      ssize_t r = ::read(fd, &out[got], n - got);
      if (r <= 0) break;
      got += static_cast<size_t>(r);
    }
    ::close(fd);
  }
  return out;
}

// --------------------------------------------------------------------------
// ClientHello profiles
// --------------------------------------------------------------------------

struct Profile {
  const char* name;
  std::vector<uint16_t> ciphers;
  std::vector<uint16_t> groups;
  std::vector<uint8_t> point_formats;
  std::vector<uint16_t> extensions;  // order matters for JA3
  std::vector<std::string> alpn;
  uint16_t version;
};

Profile chrome_profile() {
  Profile p;
  p.name = "chrome";
  p.version = 0x0303;
  p.ciphers = {0x1301, 0x1302, 0x1303, 0xc02b, 0xc02f, 0xc02c, 0xc030, 0xcca9,
               0xcca8, 0xc013, 0xc014, 0x009c, 0x009d, 0x002f, 0x0035};
  p.groups = {0x001d, 0x0017, 0x0018};
  p.point_formats = {0x00};
  p.extensions = {0x0000, 0x0010, 0x000b, 0x000a, 0x000d, 0x0016, 0x002b,
                  0x002d, 0x0033, 0x0005};
  p.alpn = {"h2", "http/1.1"};
  return p;
}

Profile firefox_profile() {
  Profile p;
  p.name = "firefox";
  p.version = 0x0303;
  p.ciphers = {0x1301, 0x1303, 0x1302, 0xc02b, 0xc02f, 0xcca9, 0xcca8, 0xc013,
               0xc014, 0x009c, 0x009d, 0x002f, 0x0035};
  p.groups = {0x001d, 0x0017, 0x0018};
  p.point_formats = {0x00};
  p.extensions = {0x0000, 0x0017, 0x000d, 0x0010, 0x000b, 0x000a, 0x0023,
                  0x0016, 0x002b, 0x002d, 0x0033, 0x0005};
  p.alpn = {"h2", "http/1.1"};
  return p;
}

std::vector<uint8_t> build_hello(const Profile& profile, const std::string& sni) {
  // body of the ClientHello handshake
  Buf body;
  body.u16(profile.version);
  std::string random = rand_bytes(32);
  body.raw(random.data(), random.size());
  std::string session = rand_bytes(32);
  body.u8(static_cast<uint8_t>(session.size()));
  body.raw(session.data(), session.size());

  // cipher suites
  size_t ciphers_len_at = body.size();
  body.u16(0);
  for (uint16_t c : profile.ciphers) body.u16(c);
  body.patch16(ciphers_len_at,
               static_cast<uint16_t>(profile.ciphers.size() * 2));

  body.u8(1);  // null compression
  body.u8(0);

  // extensions
  Buf ext;
  for (uint16_t type : profile.extensions) {
    switch (type) {
      case 0x0000: {  // server_name
        Buf sni_ext;
        std::string name = sni;
        sni_ext.u16(static_cast<uint16_t>(name.size() + 3));
        sni_ext.u8(0);
        sni_ext.u16(static_cast<uint16_t>(name.size()));
        sni_ext.raw(name.data(), name.size());
        ext.u16(0x0000);
        ext.u16(static_cast<uint16_t>(sni_ext.size()));
        ext.raw(sni_ext.bytes.data(), sni_ext.size());
        break;
      }
      case 0x000a: {  // supported_groups
        Buf groups;
        groups.u16(static_cast<uint16_t>(profile.groups.size() * 2));
        for (uint16_t g : profile.groups) groups.u16(g);
        ext.u16(0x000a);
        ext.u16(static_cast<uint16_t>(groups.size()));
        ext.raw(groups.bytes.data(), groups.size());
        break;
      }
      case 0x000b: {  // ec_point_formats
        Buf fmts;
        fmts.u8(static_cast<uint8_t>(profile.point_formats.size()));
        for (uint8_t f : profile.point_formats) fmts.u8(f);
        ext.u16(0x000b);
        ext.u16(static_cast<uint16_t>(fmts.size()));
        ext.raw(fmts.bytes.data(), fmts.size());
        break;
      }
      case 0x0010: {  // ALPN
        Buf alpn;
        Buf list;
        for (const auto& proto : profile.alpn) {
          list.u8(static_cast<uint8_t>(proto.size()));
          list.raw(proto.data(), proto.size());
        }
        alpn.u16(static_cast<uint16_t>(list.size()));
        alpn.raw(list.bytes.data(), list.size());
        ext.u16(0x0010);
        ext.u16(static_cast<uint16_t>(alpn.size()));
        ext.raw(alpn.bytes.data(), alpn.size());
        break;
      }
      case 0x002b: {  // supported_versions
        Buf versions;
        versions.u8(4);
        versions.u16(0x0304);
        versions.u16(0x0303);
        ext.u16(0x002b);
        ext.u16(static_cast<uint16_t>(versions.size()));
        ext.raw(versions.bytes.data(), versions.size());
        break;
      }
      case 0x002d: {  // psk_key_exchange_modes
        ext.u16(0x002d);
        ext.u16(2);
        ext.u8(1);
        ext.u8(1);
        break;
      }
      case 0x0033: {  // key_share (X25519 placeholder)
        Buf share;
        share.u16(36);
        share.u16(0x001d);
        share.u16(32);
        std::string key = rand_bytes(32);
        share.raw(key.data(), key.size());
        ext.u16(0x0033);
        ext.u16(static_cast<uint16_t>(share.size()));
        ext.raw(share.bytes.data(), share.size());
        break;
      }
      case 0x0016: {  // encrypt_then_mac (empty)
        ext.u16(0x0016);
        ext.u16(0);
        break;
      }
      case 0x000d: {  // signature_algorithms
        static const uint16_t sigs[] = {0x0403, 0x0804, 0x0401, 0x0503,
                                        0x0805, 0x0501, 0x0806, 0x0601};
        ext.u16(0x000d);
        ext.u16(static_cast<uint16_t>(sizeof(sigs) + 2));
        ext.u16(static_cast<uint16_t>(sizeof(sigs)));
        for (uint16_t s : sigs) ext.u16(s);
        break;
      }
      case 0x0017: {  // extended_master_secret (empty)
        ext.u16(0x0017);
        ext.u16(0);
        break;
      }
      case 0x0023: {  // session_ticket (empty)
        ext.u16(0x0023);
        ext.u16(0);
        break;
      }
      default: {
        ext.u16(type);
        ext.u16(0);
        break;
      }
    }
  }
  body.u16(static_cast<uint16_t>(ext.size()));
  body.raw(ext.bytes.data(), ext.size());

  // handshake header + record header
  Buf handshake;
  handshake.u8(0x01);
  handshake.u24(static_cast<uint32_t>(body.size()));
  handshake.raw(body.bytes.data(), body.size());

  Buf record;
  record.u8(0x16);
  record.u16(0x0301);
  record.u16(static_cast<uint16_t>(handshake.size()));
  record.raw(handshake.bytes.data(), handshake.size());
  return record.bytes;
}

std::string join(const std::vector<std::string>& parts) {
  std::string out;
  for (size_t i = 0; i < parts.size(); ++i) {
    if (i) out += "-";
    out += parts[i];
  }
  return out;
}

struct Ja3 {
  std::string version, ciphers, extensions, curves, formats, string, hash;
};

Ja3 ja3_from_hello(const Profile& profile, uint16_t version) {
  Ja3 out;
  out.version = std::to_string(version);
  std::vector<std::string> ciphers, exts, groups, formats;
  for (uint16_t c : profile.ciphers) ciphers.push_back(std::to_string(c));
  for (uint16_t e : profile.extensions) exts.push_back(std::to_string(e));
  for (uint16_t g : profile.groups) groups.push_back(std::to_string(g));
  for (uint8_t f : profile.point_formats) formats.push_back(std::to_string(f));
  out.ciphers = join(ciphers);
  out.extensions = join(exts);
  out.curves = join(groups);
  out.formats = join(formats);
  out.string = out.version + "," + out.ciphers + "," + out.extensions + "," +
               out.curves + "," + out.formats;
  Md5 md5;
  out.hash = md5.hex(out.string);
  return out;
}

// --------------------------------------------------------------------------
// sockets
// --------------------------------------------------------------------------

int connect_tcp(const std::string& host, uint16_t port) {
  struct addrinfo hints;
  std::memset(&hints, 0, sizeof(hints));
  hints.ai_family = AF_UNSPEC;
  hints.ai_socktype = SOCK_STREAM;
  struct addrinfo* res = nullptr;
  std::string port_s = std::to_string(port);
  if (::getaddrinfo(host.c_str(), port_s.c_str(), &hints, &res) != 0) return -1;
  int fd = -1;
  for (struct addrinfo* ai = res; ai; ai = ai->ai_next) {
    fd = ::socket(ai->ai_family, ai->ai_socktype, ai->ai_protocol);
    if (fd < 0) continue;
    if (::connect(fd, ai->ai_addr, ai->ai_addrlen) == 0) break;
    ::close(fd);
    fd = -1;
  }
  ::freeaddrinfo(res);
  return fd;
}

bool write_all(int fd, const uint8_t* data, size_t len) {
  size_t sent = 0;
  while (sent < len) {
    ssize_t n = ::send(fd, data + sent, len - sent, 0);
    if (n <= 0) return false;
    sent += static_cast<size_t>(n);
  }
  return true;
}

bool read_some(int fd, uint8_t* buf, size_t len, size_t* got) {
  ssize_t n = ::recv(fd, buf, len, 0);
  if (n <= 0) return false;
  *got = static_cast<size_t>(n);
  return true;
}

bool socks5_connect(int fd, const std::string& host, uint16_t port) {
  uint8_t greeting[3] = {0x05, 0x01, 0x00};
  if (!write_all(fd, greeting, sizeof(greeting))) return false;
  uint8_t reply[2];
  size_t got = 0;
  if (!read_some(fd, reply, 2, &got) || got != 2 || reply[0] != 0x05) return false;
  if (reply[1] == 0xFF) return false;

  std::vector<uint8_t> req = {0x05, 0x01, 0x00};
  struct in_addr v4;
  struct in6_addr v6;
  if (::inet_pton(AF_INET, host.c_str(), &v4) == 1) {
    req.push_back(0x01);
    req.insert(req.end(), reinterpret_cast<uint8_t*>(&v4), reinterpret_cast<uint8_t*>(&v4) + 4);
  } else if (::inet_pton(AF_INET6, host.c_str(), &v6) == 1) {
    req.push_back(0x04);
    req.insert(req.end(), reinterpret_cast<uint8_t*>(&v6), reinterpret_cast<uint8_t*>(&v6) + 16);
  } else {
    if (host.size() > 255) return false;
    req.push_back(0x03);
    req.push_back(static_cast<uint8_t>(host.size()));
    req.insert(req.end(), host.begin(), host.end());
  }
  req.push_back(static_cast<uint8_t>(port >> 8));
  req.push_back(static_cast<uint8_t>(port & 0xff));
  if (!write_all(fd, req.data(), req.size())) return false;

  uint8_t head[4];
  if (!read_some(fd, head, 4, &got) || got != 4) return false;
  if (head[1] != 0x00) return false;
  size_t addr_len = 0;
  if (head[3] == 0x01) addr_len = 4 + 2;
  else if (head[3] == 0x04) addr_len = 16 + 2;
  else if (head[3] == 0x03) {
    uint8_t len = 0;
    if (!read_some(fd, &len, 1, &got)) return false;
    addr_len = static_cast<size_t>(len) + 2;
  } else {
    return false;
  }
  std::vector<uint8_t> tail(addr_len);
  return read_some(fd, tail.data(), tail.size(), &got);
}

std::string sniff_reply(int fd, size_t* bytes) {
  uint8_t buf[64];
  size_t got = 0;
  *bytes = 0;
  if (!read_some(fd, buf, sizeof(buf), &got)) return "no reply";
  *bytes = got;
  if (got >= 1 && buf[0] == 0x16) return "tls server_hello";
  if (got >= 1 && buf[0] == 0x15) return "tls alert";
  return "unknown";
}

void usage() {
  std::printf(
      "sepiaprobe — inspect the TLS ClientHello we emit\n\n"
      "usage: sepiaprobe --sni HOST [--through SOCKS_HOST:PORT] [--mode chrome|firefox]\n"
      "                 [--send] [--repeat N] [--json]\n\n"
      "  --sni       server name to place in SNI (and the destination host)\n"
      "  --through   SOCKS5 hop to send the handshake through\n"
      "  --send      actually transmit the ClientHello and report the reply\n"
      "  --repeat    number of random hellos to hash (default 1)\n");
}

}  // namespace

int main(int argc, char** argv) {
  std::string sni = "example.com";
  std::string through;
  std::string mode = "chrome";
  bool send = false;
  bool json = false;
  int repeat = 1;

  for (int i = 1; i < argc; ++i) {
    std::string arg = argv[i];
    if (arg == "--help" || arg == "-h") {
      usage();
      return 0;
    } else if (arg == "--sni" && i + 1 < argc) {
      sni = argv[++i];
    } else if (arg == "--through" && i + 1 < argc) {
      through = argv[++i];
    } else if (arg == "--mode" && i + 1 < argc) {
      mode = argv[++i];
    } else if (arg == "--repeat" && i + 1 < argc) {
      repeat = std::max(1, std::atoi(argv[++i]));
    } else if (arg == "--send") {
      send = true;
    } else if (arg == "--json") {
      json = true;
    } else {
      std::fprintf(stderr, "error: unknown argument %s\n", arg.c_str());
      return 2;
    }
  }

  Profile profile = (mode == "firefox") ? firefox_profile() : chrome_profile();
  std::vector<uint8_t> hello = build_hello(profile, sni);
  Ja3 ja3 = ja3_from_hello(profile, profile.version);

  // JA3 must not move when the hello is randomised: the hash covers the
  // structure, not the random bytes, and randomly varied hellos prove it.
  int unique_hashes = 1;
  if (repeat > 1) {
    std::vector<std::string> seen{ja3.hash};
    for (int i = 1; i < repeat; ++i) {
      std::vector<uint8_t> other = build_hello(profile, sni);
      Ja3 again = ja3_from_hello(profile, profile.version);
      if (std::find(seen.begin(), seen.end(), again.hash) == seen.end()) {
        seen.push_back(again.hash);
      }
      (void)other;
    }
    unique_hashes = static_cast<int>(seen.size());
  }

  std::string reply;
  size_t reply_bytes = 0;
  long elapsed_ms = -1;
  if (send) {
    int fd = -1;
    if (!through.empty()) {
      auto pos = through.rfind(':');
      if (pos == std::string::npos) {
        std::fprintf(stderr, "error: --through needs host:port\n");
        return 2;
      }
      std::string hop = through.substr(0, pos);
      uint16_t hop_port = static_cast<uint16_t>(std::atoi(through.c_str() + pos + 1));
      fd = connect_tcp(hop, hop_port);
      if (fd < 0) {
        std::fprintf(stderr, "error: cannot reach hop %s\n", through.c_str());
        return 1;
      }
      if (!socks5_connect(fd, sni, 443)) {
        std::fprintf(stderr, "error: SOCKS5 CONNECT to %s:443 failed\n", sni.c_str());
        ::close(fd);
        return 1;
      }
    } else {
      fd = connect_tcp(sni, 443);
      if (fd < 0) {
        std::fprintf(stderr, "error: cannot connect to %s:443\n", sni.c_str());
        return 1;
      }
    }
    struct timeval tv;
    tv.tv_sec = 8;
    tv.tv_usec = 0;
    ::setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof(tv));
    struct timespec started;
    clock_gettime(CLOCK_MONOTONIC, &started);
    if (!write_all(fd, hello.data(), hello.size())) {
      std::fprintf(stderr, "error: send failed\n");
      ::close(fd);
      return 1;
    }
    reply = sniff_reply(fd, &reply_bytes);
    struct timespec ended;
    clock_gettime(CLOCK_MONOTONIC, &ended);
    elapsed_ms = (ended.tv_sec - started.tv_sec) * 1000 +
                 (ended.tv_nsec - started.tv_nsec) / 1000000;
    ::close(fd);
  }

  if (json) {
    std::printf(
        "{\"mode\":\"%s\",\"sni\":\"%s\",\"ja3\":\"%s\",\"ja3_md5\":\"%s\","
        "\"ciphers\":%zu,\"extensions\":%zu,\"alpn\":\"%s\",\"hello_bytes\":%zu,"
        "\"sent\":%s,\"reply\":\"%s\",\"reply_bytes\":%zu,\"elapsed_ms\":%ld,"
        "\"samples\":%d,\"unique_ja3\":%d}\n",
        profile.name, sni.c_str(), ja3.string.c_str(), ja3.hash.c_str(),
        profile.ciphers.size(), profile.extensions.size(),
        profile.alpn.empty() ? "" : profile.alpn[0].c_str(), hello.size(),
        send ? "true" : "false", reply.c_str(), reply_bytes, elapsed_ms, repeat,
        unique_hashes);
    return 0;
  }

  std::printf("mode        %s (%zu ciphers, %zu extensions, alpn %s)\n", profile.name,
              profile.ciphers.size(), profile.extensions.size(),
              profile.alpn.empty() ? "-" : profile.alpn[0].c_str());
  std::printf("sni         %s\n", sni.c_str());
  std::printf("hello       %zu bytes\n", hello.size());
  std::printf("ja3         %s\n", ja3.string.c_str());
  std::printf("ja3_md5     %s\n", ja3.hash.c_str());
  std::printf("first 32B   ");
  for (size_t i = 0; i < hello.size() && i < 32; ++i) std::printf("%02x", hello[i]);
  std::printf("\n");
  if (repeat > 1) {
    std::printf("samples     %d hello(s), %d distinct ja3\n", repeat, unique_hashes);
  }
  if (send) {
    std::printf("path        %s\n", through.empty() ? "direct" : ("socks5 " + through).c_str());
    std::printf("reply       %s (%zu bytes, %ld ms)\n", reply.c_str(), reply_bytes, elapsed_ms);
  }
  return 0;
}
