// RFC 5227 ARP probes on one explicitly selected Ethernet. No IP routing.
#include <sys/types.h>
#include <sys/ioctl.h>
#include <sys/time.h>
#include <net/bpf.h>
#include <net/if.h>
#include <net/if_dl.h>
#include <fcntl.h>
#include <unistd.h>
#include <poll.h>
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

static uint64_t milliseconds(void) {
    struct timespec ts;
    if (clock_gettime(CLOCK_MONOTONIC, &ts)) return 0;
    return (uint64_t)ts.tv_sec * 1000 + (uint64_t)ts.tv_nsec / 1000000;
}
static int conflict(const uint8_t *p, size_t n, const uint8_t *mac, const uint8_t *ip) {
    if (n < 42 || p[12] != 8 || p[13] != 6 || p[14] || p[15] != 1 ||
        p[16] != 8 || p[17] || p[18] != 6 || p[19] != 4 ||
        p[20] || (p[21] != 1 && p[21] != 2) || !memcmp(p+22, mac, 6)) return 0;
    const uint8_t zero[4] = {0};
    return !memcmp(p+28, ip, 4) || (!memcmp(p+28, zero, 4) && !memcmp(p+38, ip, 4));
}
int lrd_arp_probe(const char *name, const uint8_t *mac, const uint8_t *ip) {
    int fd = -1, result = -1;
    uint8_t *buffer = NULL;
    if (strlen(name) >= IFNAMSIZ) { errno = EINVAL; return -1; }
    for (int i = 0; i < 256; i++) {
        char path[32];
        snprintf(path, sizeof(path), "/dev/bpf%d", i);
        fd = open(path, O_RDWR | O_CLOEXEC);
        if (fd >= 0) break;
        if (errno != EBUSY) return -1;
    }
    if (fd < 0) return -1;
    struct ifreq interface;
    memset(&interface, 0, sizeof(interface));
    memcpy(interface.ifr_name, name, strlen(name));
    unsigned int one = 1, linktype = 0, length = 0;
    struct bpf_insn instructions[] = {
        BPF_STMT(BPF_LD|BPF_H|BPF_ABS, 12),
        BPF_JUMP(BPF_JMP|BPF_JEQ|BPF_K, 0x0806, 0, 1),
        BPF_STMT(BPF_RET|BPF_K, 128), BPF_STMT(BPF_RET|BPF_K, 0)
    };
    struct bpf_program filter = {4, instructions};
    if (ioctl(fd, BIOCSETIF, &interface) || ioctl(fd, BIOCGDLT, &linktype) ||
        linktype != DLT_EN10MB || ioctl(fd, BIOCIMMEDIATE, &one) ||
        ioctl(fd, BIOCSHDRCMPLT, &one) || ioctl(fd, BIOCSETF, &filter) ||
        ioctl(fd, BIOCGBLEN, &length) || length < sizeof(struct bpf_hdr) || length > 1024*1024 ||
        fcntl(fd, F_SETFL, O_NONBLOCK)) goto cleanup;
    buffer = malloc(length);
    if (!buffer) goto cleanup;
    uint8_t packet[60] = {0};
    memset(packet, 0xff, 6); memcpy(packet+6, mac, 6);
    packet[12]=8; packet[13]=6; packet[15]=1; packet[16]=8;
    packet[18]=6; packet[19]=4; packet[21]=1;
    memcpy(packet+22, mac, 6); memcpy(packet+38, ip, 4);
    unsigned int probes = 0;
    uint64_t next = milliseconds() + arc4random_uniform(1001), end = 0;
    for (;;) {
        uint64_t now = milliseconds();
        if (!now) { errno = EIO; goto cleanup; }
        if (probes < 3 && now >= next) {
            if (write(fd, packet, sizeof(packet)) != sizeof(packet)) goto cleanup;
            probes++;
            next = now + 1000 + arc4random_uniform(1001);
            if (probes == 3) end = now + 2000;
        }
        if (end && now >= end) { result = 0; break; }
        struct pollfd pfd = {fd, POLLIN, 0};
        int ready = poll(&pfd, 1, 100);
        if (ready < 0) { if (errno == EINTR) continue; goto cleanup; }
        if (pfd.revents & (POLLERR|POLLNVAL|POLLHUP)) goto cleanup;
        if (!(pfd.revents & POLLIN)) continue;
        ssize_t count = read(fd, buffer, length);
        if (count < 0) { if (errno == EAGAIN || errno == EINTR) continue; goto cleanup; }
        size_t offset = 0;
        while (offset + sizeof(struct bpf_hdr) <= (size_t)count) {
            struct bpf_hdr header;
            memcpy(&header, buffer+offset, sizeof(header));
            if (header.bh_hdrlen < sizeof(header) || header.bh_hdrlen > (size_t)count-offset ||
                header.bh_caplen > (size_t)count-offset-header.bh_hdrlen) { errno=EIO; goto cleanup; }
            if (conflict(buffer+offset+header.bh_hdrlen, header.bh_caplen, mac, ip)) { result=1; goto cleanup; }
            size_t step = BPF_WORDALIGN(header.bh_hdrlen + header.bh_caplen);
            if (!step) { errno=EIO; goto cleanup; }
            offset += step;
        }
    }
cleanup:
    { int saved = errno; free(buffer); close(fd); errno=saved; }
    return result;
}
// No privileged device is opened by this parser-only fixture entry point.
int lrd_arp_conflict_fixture(const uint8_t *p, size_t n, const uint8_t *mac, const uint8_t *ip) {
    return conflict(p,n,mac,ip);
}
