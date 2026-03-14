use crate::packet::RtpPacket;
use std::collections::BTreeMap;

/// A simple fixed-size jitter buffer that reorders incoming RTP packets
/// by sequence number and provides them in order.
pub struct JitterBuffer {
    /// Buffer indexed by sequence number
    buffer: BTreeMap<u16, RtpPacket>,
    /// Maximum number of packets to buffer
    capacity: usize,
    /// Next expected sequence number to playout
    next_playout_seq: Option<u16>,
    /// Total packets received
    packets_received: u64,
    /// Total packets dropped (buffer overflow or too late)
    packets_dropped: u64,
}

impl JitterBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            buffer: BTreeMap::new(),
            capacity,
            next_playout_seq: None,
            packets_received: 0,
            packets_dropped: 0,
        }
    }

    /// Insert a packet into the jitter buffer
    pub fn insert(&mut self, packet: RtpPacket) {
        self.packets_received += 1;
        let seq = packet.sequence_number;

        // If we have a playout sequence and this packet is too old, drop it
        if let Some(next_seq) = self.next_playout_seq {
            if Self::seq_before(seq, next_seq) {
                self.packets_dropped += 1;
                return;
            }
        }

        // If buffer is full, drop the oldest packet or reject
        if self.buffer.len() >= self.capacity {
            // Remove the oldest packet
            if let Some(&oldest_seq) = self.buffer.keys().next() {
                if Self::seq_before(oldest_seq, seq) {
                    self.buffer.remove(&oldest_seq);
                    self.packets_dropped += 1;
                } else {
                    // New packet is older than everything in buffer, drop it
                    self.packets_dropped += 1;
                    return;
                }
            }
        }

        self.buffer.insert(seq, packet);

        // Initialize playout sequence if not set
        if self.next_playout_seq.is_none() && self.buffer.len() >= self.min_fill_level() {
            self.next_playout_seq = self.buffer.keys().next().copied();
        }
    }

    /// Get the next packet in sequence order for playout.
    /// Returns None if no packet is ready.
    pub fn pop(&mut self) -> Option<RtpPacket> {
        let next_seq = self.next_playout_seq?;

        if let Some(packet) = self.buffer.remove(&next_seq) {
            self.next_playout_seq = Some(next_seq.wrapping_add(1));
            Some(packet)
        } else {
            // Packet is missing (lost); advance the playout pointer
            self.next_playout_seq = Some(next_seq.wrapping_add(1));
            None
        }
    }

    /// Peek at the next packet without removing it
    pub fn peek(&self) -> Option<&RtpPacket> {
        let next_seq = self.next_playout_seq?;
        self.buffer.get(&next_seq)
    }

    /// Get the number of packets currently buffered
    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    /// Check if the buffer is empty
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    /// Reset the buffer
    pub fn reset(&mut self) {
        self.buffer.clear();
        self.next_playout_seq = None;
    }

    /// Total packets received
    pub fn packets_received(&self) -> u64 {
        self.packets_received
    }

    /// Total packets dropped
    pub fn packets_dropped(&self) -> u64 {
        self.packets_dropped
    }

    /// Minimum fill level before starting playout
    fn min_fill_level(&self) -> usize {
        // Start playing when buffer is at least 25% full, min 1
        (self.capacity / 4).max(1)
    }

    /// Check if sequence a comes before sequence b (with wrapping)
    fn seq_before(a: u16, b: u16) -> bool {
        // Using signed comparison for wrapping sequence numbers
        let diff = a.wrapping_sub(b) as i16;
        diff < 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_packet(seq: u16, ts: u32) -> RtpPacket {
        RtpPacket::new(0, seq, ts, 0x12345678).with_payload(vec![0x7F; 160])
    }

    #[test]
    fn test_new_jitter_buffer() {
        let jb = JitterBuffer::new(10);
        assert!(jb.is_empty());
        assert_eq!(jb.len(), 0);
        assert_eq!(jb.packets_received(), 0);
        assert_eq!(jb.packets_dropped(), 0);
    }

    #[test]
    fn test_insert_and_pop_in_order() {
        let mut jb = JitterBuffer::new(10);

        // Insert packets in order
        for i in 0..5 {
            jb.insert(make_packet(i, i as u32 * 160));
        }

        // Pop should return them in order
        for i in 0..5 {
            let pkt = jb.pop().unwrap();
            assert_eq!(pkt.sequence_number, i);
        }
    }

    #[test]
    fn test_insert_out_of_order() {
        let mut jb = JitterBuffer::new(10);

        // Insert packets out of order
        jb.insert(make_packet(2, 320));
        jb.insert(make_packet(0, 0));
        jb.insert(make_packet(1, 160));
        jb.insert(make_packet(4, 640));
        jb.insert(make_packet(3, 480));

        // Pop should return them in sequence order
        assert_eq!(jb.pop().unwrap().sequence_number, 0);
        assert_eq!(jb.pop().unwrap().sequence_number, 1);
        assert_eq!(jb.pop().unwrap().sequence_number, 2);
        assert_eq!(jb.pop().unwrap().sequence_number, 3);
        assert_eq!(jb.pop().unwrap().sequence_number, 4);
    }

    #[test]
    fn test_missing_packet() {
        let mut jb = JitterBuffer::new(10);

        jb.insert(make_packet(0, 0));
        jb.insert(make_packet(1, 160));
        // Skip 2
        jb.insert(make_packet(3, 480));

        assert_eq!(jb.pop().unwrap().sequence_number, 0);
        assert_eq!(jb.pop().unwrap().sequence_number, 1);
        // Seq 2 is missing
        assert!(jb.pop().is_none());
        // Now seq 3 should be available
        assert_eq!(jb.pop().unwrap().sequence_number, 3);
    }

    #[test]
    fn test_buffer_overflow() {
        let mut jb = JitterBuffer::new(3);

        jb.insert(make_packet(0, 0));
        jb.insert(make_packet(1, 160));
        jb.insert(make_packet(2, 320));
        // Buffer is full, inserting a new packet should drop the oldest
        jb.insert(make_packet(3, 480));

        assert_eq!(jb.len(), 3);
        assert!(jb.packets_dropped() > 0);
    }

    #[test]
    fn test_late_packet_dropped() {
        let mut jb = JitterBuffer::new(10);

        jb.insert(make_packet(5, 800));
        jb.insert(make_packet(6, 960));
        jb.insert(make_packet(7, 1120));

        // Pop a few
        jb.pop(); // 5
        jb.pop(); // 6

        // Insert a packet that's already been played out
        let dropped_before = jb.packets_dropped();
        jb.insert(make_packet(4, 640));
        assert_eq!(jb.packets_dropped(), dropped_before + 1);
    }

    #[test]
    fn test_reset() {
        let mut jb = JitterBuffer::new(10);

        jb.insert(make_packet(0, 0));
        jb.insert(make_packet(1, 160));
        jb.reset();

        assert!(jb.is_empty());
        assert_eq!(jb.len(), 0);
        // Stats should be preserved
        assert_eq!(jb.packets_received(), 2);
    }

    #[test]
    fn test_peek() {
        let mut jb = JitterBuffer::new(10);

        jb.insert(make_packet(0, 0));
        jb.insert(make_packet(1, 160));

        let peeked = jb.peek().unwrap();
        assert_eq!(peeked.sequence_number, 0);
        // Peek shouldn't remove the packet
        assert_eq!(jb.len(), 2);

        let popped = jb.pop().unwrap();
        assert_eq!(popped.sequence_number, 0);
        assert_eq!(jb.len(), 1);
    }

    #[test]
    fn test_seq_wrapping() {
        let mut jb = JitterBuffer::new(10);

        // Wrap around u16 max
        jb.insert(make_packet(65534, 0));
        jb.insert(make_packet(65535, 160));
        jb.insert(make_packet(0, 320));
        jb.insert(make_packet(1, 480));

        assert_eq!(jb.pop().unwrap().sequence_number, 65534);
        assert_eq!(jb.pop().unwrap().sequence_number, 65535);
        assert_eq!(jb.pop().unwrap().sequence_number, 0);
        assert_eq!(jb.pop().unwrap().sequence_number, 1);
    }

    #[test]
    fn test_duplicate_packet() {
        let mut jb = JitterBuffer::new(10);

        jb.insert(make_packet(0, 0));
        jb.insert(make_packet(0, 0)); // Duplicate
        jb.insert(make_packet(1, 160));

        // BTreeMap replaces, so len should still be 2
        assert_eq!(jb.len(), 2);
    }

    #[test]
    fn test_empty_pop() {
        let mut jb = JitterBuffer::new(10);
        assert!(jb.pop().is_none());
    }

    #[test]
    fn test_stats() {
        let mut jb = JitterBuffer::new(10);

        for i in 0..5 {
            jb.insert(make_packet(i, i as u32 * 160));
        }

        assert_eq!(jb.packets_received(), 5);

        for _ in 0..5 {
            jb.pop();
        }

        // Insert a late packet
        jb.insert(make_packet(0, 0));
        assert_eq!(jb.packets_received(), 6);
        assert_eq!(jb.packets_dropped(), 1);
    }
}
