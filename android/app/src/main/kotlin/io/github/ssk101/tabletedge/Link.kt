package io.github.ssk101.tabletedge

import android.content.Context
import android.net.nsd.NsdManager
import android.net.nsd.NsdServiceInfo
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.provider.Settings
import android.util.Log
import org.json.JSONException
import org.json.JSONObject
import java.io.IOException
import java.io.InputStreamReader
import java.io.Reader
import java.net.ServerSocket
import java.net.Socket
import java.util.concurrent.Executors
import kotlin.concurrent.thread

/**
 * The tablet's end of the link to the PC: advertises itself on the LAN via
 * DNS-SD and accepts one PC at a time (a new connection replaces the old one).
 * Messages are JSON objects, one per line.
 *
 * Not authenticated yet: anyone on the LAN can connect. Only edge positions
 * travel over it; mouse and keyboard input stays on USB.
 */
class Link(private val context: Context, private val listener: Listener) {

    /** Called on the main thread. */
    interface Listener {
        fun onConnected(peer: String)
        fun onMessage(message: JSONObject)
        fun onDisconnected()
    }

    private val main = Handler(Looper.getMainLooper())
    private val sender = Executors.newSingleThreadExecutor()
    private val nsd = context.getSystemService(NsdManager::class.java)
    private val lock = Any()
    private var server: ServerSocket? = null
    private var stopped = false

    @Volatile
    private var client: Socket? = null

    private val registration = object : NsdManager.RegistrationListener {
        override fun onServiceRegistered(info: NsdServiceInfo) =
            log("advertised as \"${info.serviceName}\"")

        override fun onRegistrationFailed(info: NsdServiceInfo, error: Int) =
            log("could not advertise on the network (error $error)")

        override fun onServiceUnregistered(info: NsdServiceInfo) {}
        override fun onUnregistrationFailed(info: NsdServiceInfo, error: Int) {}
    }

    fun start() {
        thread(name = "tablet-edge-accept") { acceptLoop() }
    }

    fun stop() {
        synchronized(lock) {
            stopped = true
            if (server != null) runCatching { nsd.unregisterService(registration) }
            runCatching { server?.close() }
        }
        runCatching { client?.close() }
        sender.shutdown()
    }

    fun send(message: JSONObject) {
        if (sender.isShutdown) return
        val bytes = "$message\n".toByteArray()
        sender.execute {
            val socket = client ?: return@execute
            try {
                socket.getOutputStream().apply { write(bytes); flush() }
            } catch (e: IOException) {
                Log.w(EventLog.TAG, "send failed: $e")
                runCatching { socket.close() }
            }
        }
    }

    private fun acceptLoop() {
        val server = try {
            ServerSocket(0)
        } catch (e: IOException) {
            log("could not open a network port: $e")
            return
        }
        synchronized(lock) {
            if (stopped) {
                runCatching { server.close() }
                return
            }
            this.server = server
            nsd.registerService(
                NsdServiceInfo().apply {
                    serviceName = deviceName()
                    serviceType = SERVICE_TYPE
                    port = server.localPort
                },
                NsdManager.PROTOCOL_DNS_SD,
                registration,
            )
        }
        log("listening on port ${server.localPort}")
        while (true) {
            val socket = try {
                server.accept()
            } catch (_: IOException) {
                return // closed by stop()
            }
            socket.tcpNoDelay = true
            client?.let { runCatching { it.close() } }
            client = socket
            val peer = socket.inetAddress.hostAddress ?: "?"
            main.post { listener.onConnected(peer) }
            thread(name = "tablet-edge-read") { readLoop(socket) }
        }
    }

    private fun readLoop(socket: Socket) {
        try {
            val reader = InputStreamReader(socket.getInputStream(), Charsets.UTF_8)
            while (true) {
                val line = readLine(reader) ?: break
                try {
                    val message = JSONObject(line)
                    main.post { listener.onMessage(message) }
                } catch (e: JSONException) {
                    Log.w(EventLog.TAG, "ignoring malformed message: $e")
                }
            }
        } catch (e: IOException) {
            Log.i(EventLog.TAG, "connection ended: $e")
        } finally {
            runCatching { socket.close() }
            if (client === socket) {
                client = null
                main.post { listener.onDisconnected() }
            }
        }
    }

    private fun log(message: String) {
        main.post { EventLog.add(message) }
    }

    private fun deviceName(): String =
        Settings.Global.getString(context.contentResolver, Settings.Global.DEVICE_NAME) ?: Build.MODEL

    companion object {
        const val PROTOCOL_VERSION = 1
        const val SERVICE_TYPE = "_tablet-edge._tcp"
        private const val MAX_LINE = 4096

        /** Reads one line, or null at end of stream. Rejects lines over [MAX_LINE] so a peer can't exhaust memory. */
        private fun readLine(reader: Reader): String? {
            val line = StringBuilder()
            while (true) {
                val c = reader.read()
                when {
                    c == -1 -> return if (line.isEmpty()) null else line.toString()
                    c == '\n'.code -> return line.toString()
                    line.length >= MAX_LINE -> throw IOException("line longer than $MAX_LINE characters")
                    else -> line.append(c.toChar())
                }
            }
        }
    }
}
