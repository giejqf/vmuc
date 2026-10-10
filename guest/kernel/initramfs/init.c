// SPDX-License-Identifier: Apache-2.0
//
// Boot-test init for the vmuc guest kernel. Runs as PID 1 from an initramfs
// under QEMU (guest/kernel/qemu-boot-test.sh), proves that the kernel came
// up with the virtio-mmio devices and memory-reporting pieces vmuc relies
// on, prints machine-readable VMUC_* lines on the console, and powers off.
//
// Built static with aarch64-linux-gnu-gcc -static -Os by initramfs/build.sh.
// Expected virtio device IDs are passed on the kernel command line as
// vmuc.expect=net,blk,console,rng,balloon (any subset); nothing is expected
// when the parameter is absent.

#define _GNU_SOURCE
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/reboot.h>
#include <sys/stat.h>
#include <sys/utsname.h>
#include <unistd.h>

#define LOG_PREFIX "[vmuc-init] "

struct virtio_id {
    unsigned id;
    const char *name;
    const char *driver;
};

static const struct virtio_id known_ids[] = {
    { 1, "net", "virtio_net" },
    { 2, "blk", "virtio_blk" },
    { 3, "console", "virtio_console" },
    { 4, "rng", "virtio_rng" },
    { 5, "balloon", "virtio_balloon" },
    { 19, "vsock", "vmw_vsock_virtio_transport" },
    { 26, "fs", "virtiofs" },
};

static int failures;
static char fail_reason[256];

static void say(const char *fmt, ...)
{
    va_list ap;
    va_start(ap, fmt);
    fputs(LOG_PREFIX, stdout);
    vprintf(fmt, ap);
    fputc('\n', stdout);
    va_end(ap);
    fflush(stdout);
}

static void fail(const char *reason)
{
    if (failures == 0)
        snprintf(fail_reason, sizeof fail_reason, "%s", reason);
    failures++;
}

static void check(const char *name, int ok)
{
    printf("VMUC_CHECK %s %s\n", name, ok ? "ok" : "missing");
    fflush(stdout);
    if (!ok)
        fail(name);
}

static int read_file(const char *path, char *buf, size_t len)
{
    int fd = open(path, O_RDONLY | O_CLOEXEC);
    if (fd < 0)
        return -1;
    ssize_t n = read(fd, buf, len - 1);
    close(fd);
    if (n < 0)
        return -1;
    buf[n] = 0;
    while (n > 0 && (buf[n - 1] == '\n' || buf[n - 1] == ' '))
        buf[--n] = 0;
    return (int)n;
}

static int exists(const char *path)
{
    struct stat st;
    return stat(path, &st) == 0;
}

static void mount_or_warn(const char *src, const char *dst, const char *type, unsigned long flags)
{
    mkdir(dst, 0755);
    if (mount(src, dst, type, flags, NULL) != 0 && errno != EBUSY)
        say("mount %s on %s failed: %s", type, dst, strerror(errno));
}

static void reopen_console(void)
{
    // PID 1 may start without stdio if the cpio lacked /dev/console; after
    // devtmpfs is mounted the node is always there.
    int fd = open("/dev/console", O_RDWR | O_NOCTTY);
    if (fd < 0)
        return;
    dup2(fd, 0);
    dup2(fd, 1);
    dup2(fd, 2);
    if (fd > 2)
        close(fd);
    setvbuf(stdout, NULL, _IONBF, 0);
}

static const char *virtio_name(unsigned id)
{
    for (size_t i = 0; i < sizeof known_ids / sizeof known_ids[0]; i++)
        if (known_ids[i].id == id)
            return known_ids[i].name;
    return "unknown";
}

// Scans /sys/bus/virtio/devices, prints every device, and records which IDs
// are present and bound to a driver in `present` (bit per ID < 64).
static unsigned long long scan_virtio(void)
{
    unsigned long long present = 0;
    DIR *d = opendir("/sys/bus/virtio/devices");
    if (!d) {
        say("no /sys/bus/virtio/devices: %s", strerror(errno));
        return 0;
    }
    struct dirent *e;
    while ((e = readdir(d)) != NULL) {
        if (e->d_name[0] == '.')
            continue;
        char path[512], idbuf[32], link[256];
        snprintf(path, sizeof path, "/sys/bus/virtio/devices/%s/device", e->d_name);
        if (read_file(path, idbuf, sizeof idbuf) < 0)
            continue;
        unsigned id = (unsigned)strtoul(idbuf, NULL, 16);
        snprintf(path, sizeof path, "/sys/bus/virtio/devices/%s/driver", e->d_name);
        ssize_t n = readlink(path, link, sizeof link - 1);
        const char *driver = "(unbound)";
        if (n > 0) {
            link[n] = 0;
            driver = strrchr(link, '/') ? strrchr(link, '/') + 1 : link;
        }
        say("virtio %s: id %u (%s) driver %s", e->d_name, id, virtio_name(id), driver);
        if (id < 64 && n > 0)
            present |= 1ULL << id;
    }
    closedir(d);
    return present;
}

// Parses vmuc.expect=a,b,c from /proc/cmdline into a bitmask of virtio IDs.
static unsigned long long parse_expect(void)
{
    char cmdline[4096];
    if (read_file("/proc/cmdline", cmdline, sizeof cmdline) < 0)
        return 0;
    char *p = strstr(cmdline, "vmuc.expect=");
    if (!p)
        return 0;
    p += strlen("vmuc.expect=");
    char *end = strchr(p, ' ');
    if (end)
        *end = 0;
    unsigned long long mask = 0;
    for (char *tok = strtok(p, ","); tok; tok = strtok(NULL, ",")) {
        int found = 0;
        for (size_t i = 0; i < sizeof known_ids / sizeof known_ids[0]; i++) {
            if (strcmp(tok, known_ids[i].name) == 0) {
                mask |= 1ULL << known_ids[i].id;
                found = 1;
            }
        }
        if (!found)
            say("vmuc.expect: unknown device name '%s'", tok);
    }
    return mask;
}

static void write_hvc(void)
{
    // The virtio console appears as /dev/hvc0 when the test attaches a
    // virtconsole port; the harness captures the chardev to a file.
    int fd = open("/dev/hvc0", O_WRONLY | O_NOCTTY | O_NONBLOCK);
    if (fd < 0) {
        say("/dev/hvc0 not available (%s)", strerror(errno));
        return;
    }
    static const char msg[] = "VMUC_HVC_OK\n";
    if (write(fd, msg, sizeof msg - 1) < 0)
        say("write to /dev/hvc0 failed: %s", strerror(errno));
    close(fd);
    say("wrote VMUC_HVC_OK to /dev/hvc0");
}

int main(void)
{
    mount_or_warn("proc", "/proc", "proc", MS_NOSUID | MS_NOEXEC | MS_NODEV);
    mount_or_warn("sysfs", "/sys", "sysfs", MS_NOSUID | MS_NOEXEC | MS_NODEV);
    mount_or_warn("devtmpfs", "/dev", "devtmpfs", MS_NOSUID);
    reopen_console();

    struct utsname u;
    if (uname(&u) == 0)
        say("kernel %s %s", u.release, u.version);
    else
        fail("uname");

    char buf[256];
    say("nproc online: %ld", sysconf(_SC_NPROCESSORS_ONLN));
    if (read_file("/proc/cmdline", buf, sizeof buf) >= 0)
        say("cmdline: %s", buf);

    unsigned long long present = scan_virtio();
    unsigned long long expect = parse_expect();
    for (size_t i = 0; i < sizeof known_ids / sizeof known_ids[0]; i++) {
        unsigned long long bit = 1ULL << known_ids[i].id;
        if (expect & bit)
            check(known_ids[i].name, (present & bit) != 0);
    }

    // Memory-reporting pieces: balloon driver registered, page_reporting
    // compiled in (its module parameter shows up even when built in), THP
    // available, vmstat present.
    check("virtio_balloon_driver", exists("/sys/bus/virtio/drivers/virtio_balloon"));
    check("page_reporting", exists("/sys/module/page_reporting/parameters/page_reporting_order"));
    if (read_file("/sys/module/page_reporting/parameters/page_reporting_order", buf, sizeof buf) >= 0)
        say("page_reporting_order: %s", buf);
    int thp = read_file("/sys/kernel/mm/transparent_hugepage/enabled", buf, sizeof buf) >= 0;
    check("transparent_hugepage", thp);
    if (thp)
        say("transparent_hugepage/enabled: %s", buf);
    check("proc_vmstat", exists("/proc/vmstat"));
    check("cgroup2", exists("/sys/fs/cgroup") || exists("/proc/cgroups"));
    if (read_file("/proc/sys/kernel/random/boot_id", buf, sizeof buf) >= 0)
        say("boot_id: %s", buf);
    if (read_file("/proc/meminfo", buf, sizeof buf) >= 0) {
        char *nl = strchr(buf, '\n');
        if (nl)
            *nl = 0;
        say("%s", buf);
    }

    write_hvc();

    if (failures == 0)
        printf("VMUC_BOOT_OK\n");
    else
        printf("VMUC_BOOT_FAIL %s (%d check(s) failed)\n", fail_reason, failures);
    fflush(stdout);
    sync();
    reboot(RB_POWER_OFF);
    // Only reached if power-off is refused; spin so the harness times out
    // with the log intact rather than panicking on init exit.
    for (;;)
        pause();
}
