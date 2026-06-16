/// Abstract base for objects that can serialize to JSON.
abstract class Serializable {
  Map<String, dynamic> toJson();
}

/// Mixin that adds timestamp tracking to any class.
mixin Timestamped {
  DateTime get createdAt;
  DateTime get updatedAt;

  bool get isRecent => updatedAt.isAfter(
        DateTime.now().subtract(const Duration(days: 7)),
      );
}
