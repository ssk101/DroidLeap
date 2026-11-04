package io.github.ssk101.droidleap

import android.util.Log
import java.time.LocalTime
import java.time.format.DateTimeFormatter

/** In-memory event log shown in [MainActivity]. Main thread only. */
object EventLog {
    const val TAG = "DroidLeap"
    private const val MAX_LINES = 200
    private val timeFormat = DateTimeFormatter.ofPattern("HH:mm:ss.SSS")
    private val lines = ArrayDeque<String>()

    var listener: (() -> Unit)? = null

    fun add(message: String) {
        Log.i(TAG, message)
        lines.addLast("${LocalTime.now().format(timeFormat)}  $message")
        if (lines.size > MAX_LINES) lines.removeFirst()
        listener?.invoke()
    }

    fun text(): String = lines.joinToString("\n")
}
