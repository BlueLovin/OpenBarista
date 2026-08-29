(function() {
    const form = document.getElementById('uploadForm');
    const fileInput = document.getElementById('firmwareFile');

    // Drag-drop support
    function handleDrop(e) {
        e.preventDefault();
        const dt = e.dataTransfer;
        if (dt.files.length > 0) fileInput.files = dt.files;
    }
    window.addEventListener('dragover', handleDrop);
    window.addEventListener('drop', handleDrop);

    // Validate before submit
    form.addEventListener('submit', async function(e) {
        // Always keep the browser from doing its own (urlencoded) form POST;
        // the firmware is streamed via fetch() below instead.
        e.preventDefault();
        const files = Array.from(fileInput.files || []);
        if (files.length !== 1) return;

        const f = files[0];
        if (!f.name.toLowerCase().endsWith('.bin')) {
            alert("Please upload a .bin file only.");
            return;
        }
        if (f.size > 1900544) { // 0x1D0000 = OTA slot size per partitions_two_ota.csv
            alert("File too large — must be ≤ 1.9 MB.");
            return;
        }

        try {
            const res = await fetch('/api/firmware-upload', { method: 'POST', body: f });
            if (!res.ok) {
                const json = await res.json().catch(() => ({}));
                throw new Error(json.error || `Server responded ${res.status}`);
            }
            const json = await res.json();
            console.log('Flash complete:', json);
            alert("Firmware flashed successfully. Device is rebooting (~1 min for flash erase/write is normal).");
        } catch (err) {
            console.error(err);
            alert("Upload failed: " + err.message);
        }
    });

})();

