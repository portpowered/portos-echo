// Android Wi-Fi reflection calls adapted from echo-dot-assist (MIT),
// portpowered/echo-dot-assist@7e0c758 tools/mkwifictl.py.
// The copyright and licence ship with echo-bridge notices.
import android.net.ConnectivityManager;
import android.net.LinkAddress;
import android.net.LinkProperties;
import android.net.RouteInfo;
import android.net.wifi.ScanResult;
import android.net.wifi.SupplicantState;
import android.net.wifi.WifiConfiguration;
import android.net.wifi.WifiInfo;
import android.os.SystemClock;
import java.io.BufferedReader;
import java.io.InputStreamReader;
import java.net.InetAddress;
import java.nio.charset.StandardCharsets;
import java.util.HashMap;
import java.util.List;
import java.util.Map;
import org.json.JSONArray;
import org.json.JSONObject;

public final class EchoWifi {
    private static final String PACKAGE = "android";

    public static void main(String[] args) {
        Thread watchdog = new Thread(() -> {
            SystemClock.sleep(8000);
            Runtime.getRuntime().halt(1);
        });
        watchdog.setDaemon(true);
        watchdog.start();
        try {
            String op = new BufferedReader(new InputStreamReader(System.in, StandardCharsets.UTF_8)).readLine();
            Object wifi = service("wifi", "android.net.wifi.IWifiManager");
            JSONObject reply;
            if ("status".equals(op)) {
                reply = status(wifi);
            } else if ("scan".equals(op)) {
                reply = scan(wifi);
            } else {
                System.exit(1);
                return;
            }
            System.out.println(reply.toString());
        } catch (Exception e) {
            // Framework diagnostics can include network names. The caller
            // maps absent or invalid output to the fixed unavailable result.
            System.exit(1);
        }
        System.exit(0);
    }

    private static Object service(String name, String type) throws Exception {
        Object binder = Class.forName("android.os.ServiceManager")
            .getMethod("getService", String.class).invoke(null, name);
        return Class.forName(type + "$Stub").getMethod("asInterface", Class.forName("android.os.IBinder"))
            .invoke(null, binder);
    }

    private static Object call(Object service, String name, Class<?>[] types, Object... args) throws Exception {
        java.lang.reflect.Method method = service.getClass().getMethod(name, types);
        method.setAccessible(true);
        return method.invoke(service, args);
    }

    private static JSONObject status(Object wifi) throws Exception {
        WifiInfo info = (WifiInfo) call(wifi, "getConnectionInfo", new Class<?>[]{String.class}, PACKAGE);
        String ssid = info == null ? null : ssid(info.getSSID());
        if (info == null || info.getSupplicantState() != SupplicantState.COMPLETED || info.getNetworkId() < 0) {
            ssid = null;
        }
        boolean reachable = ssid != null && usableRoute();
        return new JSONObject().put("ssid", ssid == null ? JSONObject.NULL : ssid).put("reachable", reachable);
    }

    private static boolean usableRoute() throws Exception {
        Object connectivity = service("connectivity", "android.net.IConnectivityManager");
        LinkProperties link = (LinkProperties) call(connectivity, "getLinkPropertiesForType",
            new Class<?>[]{int.class}, ConnectivityManager.TYPE_WIFI);
        if (link == null) return false;
        for (RouteInfo route : link.getRoutes()) {
            InetAddress gateway = route.getGateway();
            if (!route.isDefaultRoute() || gateway == null || gateway.isAnyLocalAddress()) continue;
            for (LinkAddress addr : link.getLinkAddresses()) {
                if (addr.getAddress().getClass() == gateway.getClass()
                    && !addr.getAddress().isLinkLocalAddress()
                    && !addr.getAddress().isLoopbackAddress()
                    && !addr.getAddress().isAnyLocalAddress()) return true;
            }
        }
        return false;
    }

    private static JSONObject scan(Object wifi) throws Exception {
        long requested = SystemClock.elapsedRealtime();
        boolean started = (Boolean) call(wifi, "startScan", new Class<?>[]{String.class}, PACKAGE);
        List<?> results = scanResults(wifi, requested, started);
        Map<String, Integer> saved = saved(wifi);
        JSONArray rows = new JSONArray();
        for (Object item : results) {
            ScanResult result = (ScanResult) item;
            if (result.SSID == null || result.SSID.isEmpty()) continue;
            Integer id = saved.get(result.SSID);
            rows.put(new JSONObject().put("ssid", result.SSID).put("signalDbm", result.level)
                .put("secured", secured(result.capabilities)).put("networkId", id == null ? JSONObject.NULL : id));
            if (rows.length() == 512) break;
        }
        return new JSONObject().put("networks", rows);
    }

    private static List<?> scanResults(Object wifi, long requested, boolean started) throws Exception {
        while (true) {
            List<?> results = (List<?>) call(wifi, "getScanResults", new Class<?>[]{String.class}, PACKAGE);
            if (!started || SystemClock.elapsedRealtime() >= requested + 3500) return results;
            for (Object item : results) {
                if (((ScanResult) item).timestamp / 1000 >= requested) return results;
            }
            SystemClock.sleep(200);
        }
    }

    private static Map<String, Integer> saved(Object wifi) throws Exception {
        Object parcel = call(wifi, "getConfiguredNetworks", new Class<?>[0]);
        List<?> configs = (List<?>) call(parcel, "getList", new Class<?>[0]);
        Map<String, Integer> ids = new HashMap<>();
        for (Object item : configs) {
            WifiConfiguration config = (WifiConfiguration) item;
            String name = ssid(config.SSID);
            if (name != null && config.networkId >= 0) ids.putIfAbsent(name, config.networkId);
        }
        return ids;
    }

    private static String ssid(String name) {
        if (name == null || "<unknown ssid>".equals(name)) return null;
        if (name.length() >= 2 && name.startsWith("\"") && name.endsWith("\"")) {
            return name.substring(1, name.length() - 1);
        }
        return name;
    }

    private static boolean secured(String capabilities) {
        return capabilities != null && (capabilities.contains("WEP") || capabilities.contains("WPA")
            || capabilities.contains("PSK") || capabilities.contains("SAE") || capabilities.contains("EAP")
            || capabilities.contains("OWE"));
    }
}
