package io.github.ssk101.droidleap

import android.accessibilityservice.AccessibilityService
import android.content.res.Configuration
import android.hardware.display.DisplayManager
import android.view.Display
import android.view.WindowManager
import android.view.accessibility.AccessibilityEvent
import org.json.JSONObject

/**
 * Runs the link to the PC and shows the edge strip the PC asks for. An
 * accessibility service so the strip can sit above the status bar; it
 * subscribes to no accessibility events and can't read screen content.
 */
class EdgeAccessibilityService : AccessibilityService(), Link.Listener {

    private var link: Link? = null
    private var strip: EdgeStrip? = null

    override fun onServiceConnected() {
        strip = EdgeStrip(this, ::onHover)
        link = Link(this, this).also { it.start() }
        EventLog.add("service started, waiting for the PC")
    }

    override fun onDestroy() {
        link?.stop()
        link = null
        strip?.hide()
        strip = null
        EventLog.add("service stopped")
        super.onDestroy()
    }

    override fun onConfigurationChanged(newConfig: Configuration) {
        super.onConfigurationChanged(newConfig)
        link?.send(displayMessage("display"))
    }

    override fun onConnected(peer: String) {
        EventLog.add("PC connected from $peer")
        link?.send(displayMessage("hello").put("version", Link.PROTOCOL_VERSION).put("app", appVersion()))
    }

    override fun onMessage(message: JSONObject) {
        when (message.optString("type")) {
            "hello" -> {
                val version = message.optInt("version")
                if (version != Link.PROTOCOL_VERSION) {
                    EventLog.add("PC speaks protocol $version, this app ${Link.PROTOCOL_VERSION}: update one of them")
                    return
                }
                val edge = Edge.fromWire(message.optString("edge"))
                if (edge == null) {
                    EventLog.add("PC asked for unknown edge \"${message.optString("edge")}\"")
                    return
                }
                strip?.show(edge)
                EventLog.add("watching the ${edge.wireName} edge")
            }
            else -> EventLog.add("ignoring message: $message")
        }
    }

    override fun onDisconnected() {
        strip?.hide()
        EventLog.add("PC disconnected")
    }

    private fun onHover(edge: Edge, state: HoverState, along: Double) {
        if (state != HoverState.MOVE) EventLog.add("pointer ${state.wireName} ${edge.wireName} edge at %.3f".format(along))
        link?.send(
            JSONObject()
                .put("type", "edge")
                .put("edge", edge.wireName)
                .put("state", state.wireName)
                .put("along", along),
        )
    }

    private fun displayMessage(type: String): JSONObject {
        val bounds = getSystemService(WindowManager::class.java).maximumWindowMetrics.bounds
        val display = getSystemService(DisplayManager::class.java).getDisplay(Display.DEFAULT_DISPLAY)
        return JSONObject()
            .put("type", type)
            .put("width", bounds.width())
            .put("height", bounds.height())
            .put("rotation", display.rotation * 90)
    }

    private fun appVersion(): String = packageManager.getPackageInfo(packageName, 0).versionName ?: "?"

    override fun onAccessibilityEvent(event: AccessibilityEvent?) {}

    override fun onInterrupt() {}
}
