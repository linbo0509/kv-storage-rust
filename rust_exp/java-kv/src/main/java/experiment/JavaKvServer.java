package experiment;

import java.io.BufferedReader;
import java.io.BufferedWriter;
import java.io.IOException;
import java.io.InputStreamReader;
import java.io.OutputStreamWriter;
import java.net.InetSocketAddress;
import java.net.ServerSocket;
import java.net.Socket;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.concurrent.atomic.AtomicLong;
import java.util.concurrent.locks.ReentrantLock;

/**
 * 实验 A 的 Java 纯内存 KV 服务器。
 *
 * <p>它与 Rust 实验服务器采用相同的逐行 TCP 协议、线程模型、共享锁和 HashMap，
 * 并刻意关闭磁盘持久化与逐条日志，使实验主要观察内存分配和 GC 对尾延迟的影响。</p>
 */
public final class JavaKvServer {
    private static final Map<String, String> STORE = new HashMap<>();
    private static final ReentrantLock STORE_LOCK = new ReentrantLock();
    private static final AtomicLong NEXT_WORKER_ID = new AtomicLong();

    private JavaKvServer() {
    }

    public static void main(String[] args) {
        try {
            Address address = parseAddress(args);
            run(address);
        } catch (Exception error) {
            System.err.println("Java 纯内存服务器错误：" + error.getMessage());
            System.exit(1);
        }
    }

    private static void run(Address address) throws IOException {
        try (ServerSocket server = new ServerSocket()) {
            server.setReuseAddress(true);
            server.bind(new InetSocketAddress(address.host(), address.port()));
            System.out.printf("Java 纯内存 KV 已启动：%s:%d%n", address.host(), address.port());
            System.out.println("实验模式：G1 GC、无 WAL、无快照、无逐条日志");

            while (true) {
                Socket socket = server.accept();
                long workerId = NEXT_WORKER_ID.incrementAndGet();
                Thread worker = new Thread(() -> {
                    try {
                        handleClient(socket);
                    } catch (IOException error) {
                        System.err.println("客户端会话错误：" + error.getMessage());
                    }
                }, "java-kv-client-" + workerId);
                worker.start();
            }
        }
    }

    private static void handleClient(Socket socket) throws IOException {
        try (socket;
             BufferedReader reader = new BufferedReader(new InputStreamReader(
                     socket.getInputStream(), StandardCharsets.UTF_8));
             BufferedWriter writer = new BufferedWriter(new OutputStreamWriter(
                     socket.getOutputStream(), StandardCharsets.UTF_8))) {
            socket.setTcpNoDelay(true);
            String line;
            while ((line = reader.readLine()) != null) {
                ParsedResponse response = execute(line);
                writer.write(response.text());
                writer.newLine();
                writer.flush();
                if (response.closeConnection()) {
                    return;
                }
            }
        }
    }

    private static ParsedResponse execute(String line) {
        String trimmed = line.trim();
        if (trimmed.isEmpty()) {
            return response(error("INVALID_COMMAND", "命令不能为空"));
        }

        int separator = findWhitespace(trimmed);
        String command = (separator < 0 ? trimmed : trimmed.substring(0, separator))
                .toUpperCase(Locale.ROOT);
        String arguments = separator < 0 ? "" : trimmed.substring(separator).stripLeading();

        return switch (command) {
            case "SET" -> response(set(arguments));
            case "UPDATE" -> response(update(arguments));
            case "GET" -> response(get(arguments));
            case "DELETE" -> response(delete(arguments));
            case "KEYS" -> response(arguments.isEmpty()
                    ? keys()
                    : error("INVALID_COMMAND", "参数过多"));
            case "STATUS" -> response(arguments.isEmpty()
                    ? status()
                    : error("INVALID_COMMAND", "参数过多"));
            case "SAVE" -> response(arguments.isEmpty()
                    ? error("UNSUPPORTED", "纯内存实验模式不支持 SAVE")
                    : error("INVALID_COMMAND", "参数过多"));
            case "QUIT", "EXIT" -> new ParsedResponse(
                    arguments.isEmpty() ? "OK\tBYE" : error("INVALID_COMMAND", "参数过多"),
                    arguments.isEmpty());
            default -> response(error("INVALID_COMMAND", "未知命令：" + command));
        };
    }

    private static String set(String arguments) {
        KeyValue pair = parseKeyValue(arguments);
        if (pair == null) {
            return error("INVALID_COMMAND", "SET 需要 key 和 value");
        }
        STORE_LOCK.lock();
        try {
            STORE.put(pair.key(), pair.value());
            return "OK";
        } finally {
            STORE_LOCK.unlock();
        }
    }

    private static String update(String arguments) {
        KeyValue pair = parseKeyValue(arguments);
        if (pair == null) {
            return error("INVALID_COMMAND", "UPDATE 需要 key 和 value");
        }
        STORE_LOCK.lock();
        try {
            if (!STORE.containsKey(pair.key())) {
                return error("NOT_FOUND", pair.key());
            }
            STORE.put(pair.key(), pair.value());
            return "OK";
        } finally {
            STORE_LOCK.unlock();
        }
    }

    private static String get(String arguments) {
        String key = parseSingleKey(arguments);
        if (key == null) {
            return error("INVALID_COMMAND", "GET 需要且只需要一个 key");
        }
        STORE_LOCK.lock();
        try {
            String value = STORE.get(key);
            return value == null ? error("NOT_FOUND", key) : "VALUE\t" + escape(value);
        } finally {
            STORE_LOCK.unlock();
        }
    }

    private static String delete(String arguments) {
        String key = parseSingleKey(arguments);
        if (key == null) {
            return error("INVALID_COMMAND", "DELETE 需要且只需要一个 key");
        }
        STORE_LOCK.lock();
        try {
            return STORE.remove(key) == null ? error("NOT_FOUND", key) : "OK";
        } finally {
            STORE_LOCK.unlock();
        }
    }

    private static String keys() {
        STORE_LOCK.lock();
        try {
            List<String> keys = new ArrayList<>(STORE.keySet());
            keys.sort(String::compareTo);
            StringBuilder response = new StringBuilder("KEYS\t").append(keys.size());
            for (String key : keys) {
                response.append('\t').append(escape(key));
            }
            return response.toString();
        } finally {
            STORE_LOCK.unlock();
        }
    }

    private static String status() {
        STORE_LOCK.lock();
        try {
            return "STATUS\trunning\tkeys=" + STORE.size();
        } finally {
            STORE_LOCK.unlock();
        }
    }

    private static KeyValue parseKeyValue(String arguments) {
        int separator = findWhitespace(arguments);
        if (separator <= 0) {
            return null;
        }
        String key = arguments.substring(0, separator);
        String value = arguments.substring(separator).stripLeading();
        return value.isEmpty() ? null : new KeyValue(key, value);
    }

    private static String parseSingleKey(String arguments) {
        if (arguments.isEmpty() || findWhitespace(arguments) >= 0) {
            return null;
        }
        return arguments;
    }

    private static int findWhitespace(String value) {
        for (int index = 0; index < value.length(); index++) {
            if (Character.isWhitespace(value.charAt(index))) {
                return index;
            }
        }
        return -1;
    }

    private static String error(String code, String message) {
        return "ERR\t" + escape(code) + "\t" + escape(message);
    }

    private static String escape(String value) {
        return value
                .replace("\\", "\\\\")
                .replace("\t", "\\t")
                .replace("\r", "\\r")
                .replace("\n", "\\n");
    }

    private static ParsedResponse response(String text) {
        return new ParsedResponse(text, false);
    }

    private static Address parseAddress(String[] args) {
        String address = "127.0.0.1:7879";
        if (args.length == 2 && "--addr".equals(args[0])) {
            address = args[1];
        } else if (args.length != 0) {
            throw new IllegalArgumentException("用法：JavaKvServer [--addr IP:PORT]");
        }

        int separator = address.lastIndexOf(':');
        if (separator <= 0 || separator == address.length() - 1) {
            throw new IllegalArgumentException("地址必须是 IP:PORT");
        }
        String host = address.substring(0, separator);
        int port = Integer.parseInt(address.substring(separator + 1));
        if (port < 1 || port > 65_535) {
            throw new IllegalArgumentException("端口必须在 1 到 65535 之间");
        }
        return new Address(host, port);
    }

    private record Address(String host, int port) {
    }

    private record KeyValue(String key, String value) {
    }

    private record ParsedResponse(String text, boolean closeConnection) {
    }
}
