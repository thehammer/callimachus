/// String utility extensions.
extension StringX on String {
  /// Returns true when the string is empty or contains only whitespace.
  bool get isBlank => trim().isEmpty;

  /// Truncates to [maxLength] characters with an ellipsis.
  String truncate(int maxLength) =>
      length <= maxLength ? this : '${substring(0, maxLength)}…';
}

/// Converts a string to a URL-friendly slug.
String slugify(String s) {
  return s
      .toLowerCase()
      .replaceAll(RegExp(r'[^a-z0-9]+'), '-')
      .replaceAll(RegExp(r'^-|-$'), '');
}
